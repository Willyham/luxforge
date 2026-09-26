//! The owner-driven half of the suite: one module's whole editing surface through the JSON method
//! table, as two independent clients reach it, then the same catalog served with the module
//! unavailable and reopened. Every step names what it shows, so a failure says which property of
//! which module broke.
use super::{
    ACTOR, Checked, Evidence, ensure, mutation,
    pixels::{self, Sources},
    shape::{Field, FieldPatch},
    within,
};
use luxforge_core::{ClientId, LayerId, ModuleRegistry, Recipe};
use luxforge_testkit::client::{Owner, registry_without};
use serde_json::{Map, Value, json};
use std::path::Path;

/// What the journey leaves in the catalog, which the unavailable-provider and reopen checks read
/// back.
pub struct Final {
    asset: Value,
    entry: Value,
    revision: u64,
    recipe: Recipe,
    rows: Value,
    probes: Vec<(u32, u32)>,
    samples: Vec<Value>,
    identity: Value,
}

/// The committed state a step that must change nothing is compared against: the revision, the
/// newest history entry, the owner's event sequence, the current entry and its recipe.
struct Quiet {
    revision: u64,
    head: u64,
    events: u64,
    entry: Value,
    recipe: Recipe,
}

impl Quiet {
    fn capture(owner: &Owner, client: ClientId, asset: &Value) -> Checked<Self> {
        let state = owner.state(client, asset)?;
        Ok(Self {
            revision: owner.revision(client, asset)?,
            head: owner.head(client, asset)?,
            events: owner.events(client)?,
            entry: state["current_entry"]["id"].clone(),
            recipe: owner.recipe(client, asset)?,
        })
    }

    fn unchanged(&self, owner: &Owner, client: ClientId, asset: &Value, what: &str) -> Checked {
        let now = Self::capture(owner, client, asset)?;
        ensure(
            now.revision == self.revision,
            format!(
                "{what} moved the revision {} -> {}",
                self.revision, now.revision
            ),
        )?;
        ensure(
            now.head == self.head,
            format!("{what} wrote a history entry"),
        )?;
        ensure(
            now.events == self.events,
            format!("{what} announced {} event(s)", now.events - self.events),
        )?;
        ensure(
            now.entry == self.entry,
            format!("{what} moved the current entry"),
        )?;
        ensure(
            now.recipe == self.recipe,
            format!("{what} changed the recipe"),
        )
    }
}

fn object(value: &Value) -> Checked<Map<String, Value>> {
    value
        .as_object()
        .cloned()
        .ok_or_else(|| format!("{value} is not an object"))
}

/// One module action through its generated `edit.<action>` method, against the current revision.
fn edit(
    owner: &Owner,
    client: ClientId,
    asset: &Value,
    action: &str,
    fields: &Value,
    mask: Option<&Value>,
) -> Checked<Value> {
    let revision = owner.revision(client, asset)?;
    let mut params = Value::Object(object(fields)?);
    params["asset_id"] = asset.clone();
    params["mutation"] = mutation(revision, action);
    if let Some(mask) = mask {
        params["mask"] = mask.clone();
    }
    owner.call(client, &format!("edit.{action}"), params)
}

fn applied(result: &Value, what: &str) -> Checked {
    ensure(
        result["outcome"] == json!("applied") && !result["created_entry_id"].is_null(),
        format!("{what} answered {result}"),
    )
}

fn no_op(result: &Value, what: &str) -> Checked {
    ensure(
        result["outcome"] == json!("no-op") && result["created_entry_id"].is_null(),
        format!("{what} answered {result}, not a no-op"),
    )
}

/// The rows `recipe.describe` reports for the module's effect: the global one first, then each
/// mask's.
fn rows(
    owner: &Owner,
    client: ClientId,
    asset: &Value,
    module: &FieldPatch,
) -> Checked<Vec<Value>> {
    let described = owner.describe(client, asset)?;
    Ok(described["layers"]
        .as_array()
        .ok_or("recipe.describe answered no layers")?
        .iter()
        .filter(|row| row["effect"] == json!(module.effect.id))
        .cloned()
        .collect())
}

/// The one row of the effect for a target, where the global target is the row without a mask.
fn row<'r>(rows: &'r [Value], mask: Option<&Value>) -> Checked<&'r Value> {
    let found: Vec<&Value> = rows
        .iter()
        .filter(|row| match mask {
            Some(mask) => &row["mask"] == mask,
            None => row["mask"].is_null(),
        })
        .collect();
    match found.as_slice() {
        [row] => Ok(row),
        _ => Err(format!(
            "{} rows for the {} target, where one layer per target is the contract: {rows:?}",
            found.len(),
            if mask.is_some() { "masked" } else { "global" }
        )),
    }
}

/// Every declared field's value in a row, compared numerically, with the moved fields named in
/// `moved` and every other field at its default.
fn expect_values(
    module: &FieldPatch,
    row: &Value,
    moved: &Map<String, Value>,
    what: &str,
) -> Checked {
    let values = row["values"]
        .as_object()
        .ok_or_else(|| format!("{what}: the row reports no values: {row}"))?;
    ensure(
        values.len() == module.fields.len(),
        format!(
            "{what}: the row reports {} values for {} fields",
            values.len(),
            module.fields.len()
        ),
    )?;
    for field in &module.fields {
        let expected = moved
            .get(&field.name)
            .and_then(Value::as_f64)
            .unwrap_or(field.default);
        ensure(
            values.get(&field.name).and_then(Value::as_f64) == Some(expected),
            format!(
                "{what}: {} reads {:?}, expected {expected}",
                field.name,
                values.get(&field.name)
            ),
        )?;
    }
    Ok(())
}

/// Set the global layer to `payload`, which may already be what it holds.
fn moved_to(
    owner: &Owner,
    client: ClientId,
    asset: &Value,
    module: &FieldPatch,
    payload: &Value,
    what: &str,
) -> Checked {
    let result = edit(owner, client, asset, &module.set, payload, None)?;
    if result["outcome"] != json!("no-op") {
        applied(&result, what)?;
    }
    expect_values(
        module,
        row(&rows(owner, client, asset, module)?, None)?,
        &object(payload)?,
        what,
    )
}

/// The values a row holds, as a patch that would set them.
fn values_of(row: &Value) -> Checked<Map<String, Value>> {
    object(&row["values"])
}

/// The history entry a change created: its label follows the declared rules and it stores the
/// patch exactly as sent.
fn expect_entry(
    owner: &Owner,
    client: ClientId,
    asset: &Value,
    module: &FieldPatch,
    result: &Value,
    action: &str,
    sent: &Map<String, Value>,
) -> Checked<String> {
    let entry = owner.call(
        client,
        "history.inspect",
        json!({"asset_id": asset, "entry_id": result["created_entry_id"]}),
    )?;
    let label = entry["label"]
        .as_str()
        .ok_or_else(|| format!("history.inspect answered no label: {entry}"))?
        .to_owned();
    let expected = if action == module.reset {
        super::shape::Label::Exactly(format!("Reset {}", module.title))
    } else {
        module.expected_label(sent)?
    };
    ensure(
        expected.matches(&label),
        format!("the entry is labelled {label:?}, expected {expected:?}"),
    )?;
    ensure(
        entry["action_id"] == json!(action),
        format!("the entry stores the action {}", entry["action_id"]),
    )?;
    let stored = object(&entry["parameters"])?;
    ensure(
        stored.len() == sent.len()
            && sent
                .iter()
                .all(|(name, value)| stored.get(name).and_then(Value::as_f64) == value.as_f64()),
        format!("the entry stores {stored:?}, not the patch as sent {sent:?}"),
    )?;
    Ok(label)
}

/// `render.sample` through the owner equals an in-process render of the recipe the owner reports,
/// at every probe, and the same stack samples equal to its render on both paths.
fn served_equals_render(
    owner: &Owner,
    client: ClientId,
    asset: &Value,
    registry: &ModuleRegistry,
    sources: &Sources,
) -> Checked<Value> {
    let recipe = owner.recipe(client, asset)?;
    let raster = pixels::raster(registry, sources, &recipe)?;
    let probes = pixels::probes(raster.width, raster.height);
    let served = owner.samples(client, asset, &probes, None)?;
    for (&(x, y), served) in probes.iter().zip(&served) {
        ensure(
            *served == json!(raster.pixel(x, y)),
            format!(
                "render.sample answered {served} at ({x}, {y}) where the recipe renders {:?}",
                raster.pixel(x, y)
            ),
        )?;
    }
    Ok(
        json!({"served_probes": probes.len(), "paths": pixels::sample_equals_render(registry, sources, &recipe)?}),
    )
}

/// The current stack is exactly `expected` and `render.sample` answers its pixels.
fn expect_current(
    owner: &Owner,
    client: ClientId,
    asset: &Value,
    expected: &Recipe,
    pixels: &[Value],
    probes: &[(u32, u32)],
    what: &str,
) -> Checked {
    let recipe = owner.recipe(client, asset)?;
    ensure(
        recipe.layers == expected.layers && recipe.masks == expected.masks,
        format!("{what}: the current stack is not the one expected"),
    )?;
    ensure(
        owner.samples(client, asset, probes, None)? == pixels,
        format!("{what}: render.sample does not answer the stack's pixels"),
    )
}

/// A value of `field` different from `current`.
fn other_than(field: &Field, current: f64) -> f64 {
    if current == field.high() {
        field.low()
    } else {
        field.high()
    }
}

pub fn journey(
    module: &FieldPatch,
    sources: &Sources,
    fixture: &Path,
    catalog: &Path,
    evidence: &mut Evidence,
) -> Checked<Final> {
    let registry = ModuleRegistry::builtin();
    let owner = Owner::start(catalog, ModuleRegistry::builtin(), ACTOR)?;
    let editor = owner.client();
    let agent = owner.client();
    let set = module.set.as_str();
    let reset = module.reset.as_str();
    let source_probes = pixels::probes(sources.byte.width, sources.byte.height);
    let source_pixels: Vec<Value> = source_probes
        .iter()
        .map(|probe| sources.source_pixel(*probe))
        .collect();
    // The field a gesture moves: the first one that changes the image on its own at a probe, so a
    // first set of it commits a layer (the vignette's shape fields alone are neutral at amount 0)
    // and a draft of it previews pixels a client can tell from the source's.
    let mut lead = None;
    for (field, payload) in module.single_field_payloads() {
        let layer = pixels::layer(module, &payload);
        if registry.layer_neutral(&layer) {
            continue;
        }
        let raster = pixels::raster(&registry, sources, &pixels::stack(vec![layer]))?;
        let moved = source_probes
            .iter()
            .zip(&source_pixels)
            .any(|(&(x, y), source)| json!(raster.pixel(x, y)) != *source);
        if moved {
            lead = Some(field.clone());
            break;
        }
    }
    let lead = lead.ok_or("no single field changes a probe pixel on its own")?;
    let second = module
        .fields
        .iter()
        .find(|field| field.name != lead.name)
        .unwrap_or(&lead)
        .clone();
    let patch = |pairs: &[(&Field, f64)]| -> Map<String, Value> {
        pairs
            .iter()
            .map(|(field, value)| (field.name.clone(), json!(value)))
            .collect()
    };

    within("discovery", || {
        let listed = owner.call(editor, "module.list", json!({}))?;
        let served = listed["modules"]
            .as_array()
            .ok_or("module.list answered no modules")?
            .iter()
            .find(|served| served["id"] == json!(module.id))
            .cloned()
            .ok_or("module.list does not list the module")?;
        let declared = serde_json::to_value(&module.descriptor)
            .map_err(|error| format!("the descriptor does not serialize: {error}"))?;
        ensure(
            served == declared,
            format!("module.list serves {served} while the registry holds {declared}"),
        )?;
        ensure(
            served["availability"]["kind"] == json!("available"),
            format!("the module reports availability {}", served["availability"]),
        )?;
        ensure(
            module.effect.single,
            "the effect does not declare that it owns one layer per target",
        )?;
        let schema = owner.call(editor, "schema.list", json!({}))?;
        let methods = &schema["methods"];
        // Every host method this journey drives is discoverable before it is used.
        let mut used = vec![
            "session.state",
            "draft.begin",
            "draft.set",
            "draft.read",
            "draft.cancel",
            "draft.commit",
            "draft.reapply",
            "render.sample",
            "recipe.describe",
            "history.list",
            "history.inspect",
            "history.undo",
            "history.redo",
            "history.restore",
            "preview.select",
            "preview.return-current",
            "events.since",
            "analysis.request",
            "analysis.read",
        ];
        if module.effect.maskable {
            used.push("mask.create-radial");
        }
        for method in &used {
            ensure(
                methods.get(method).is_some(),
                format!("{method} is not discoverable"),
            )?;
        }
        let set_method = &methods[format!("edit.{set}")];
        let reset_method = &methods[format!("edit.{reset}")];
        ensure(
            set_method["patch"] == json!(true) && set_method["mutates"] == json!(true),
            format!("edit.{set} is described as {set_method}"),
        )?;
        let optional = set_method["optional"]
            .as_object()
            .ok_or_else(|| format!("edit.{set} lists no optional fields"))?;
        let required = set_method["required"]
            .as_array()
            .ok_or_else(|| format!("edit.{set} lists no required fields"))?;
        for field in &module.fields {
            ensure(
                optional.contains_key(&field.name) && !required.contains(&json!(field.name)),
                format!("edit.{set} does not list {} as optional", field.name),
            )?;
        }
        let names: Vec<&str> = set_method["parameters"]
            .as_array()
            .ok_or_else(|| format!("edit.{set} declares no parameters"))?
            .iter()
            .filter_map(|parameter| parameter["name"].as_str())
            .collect();
        ensure(
            names
                == module
                    .fields
                    .iter()
                    .map(|field| field.name.as_str())
                    .collect::<Vec<_>>(),
            format!("edit.{set} declares {names:?}"),
        )?;
        ensure(
            reset_method["patch"] == json!(false)
                && reset_method["parameters"] == json!([])
                && reset_method["mutates"] == json!(true),
            format!("edit.{reset} is described as {reset_method}"),
        )?;
        for method in [set_method, reset_method] {
            ensure(
                method["optional"].get("mask").is_some() == module.effect.maskable,
                format!(
                    "the mask target is {}listed on {method} although the effect is {}maskable",
                    if method["optional"].get("mask").is_some() {
                        ""
                    } else {
                        "not "
                    },
                    if module.effect.maskable { "" } else { "not " }
                ),
            )?;
        }
        for group in &module.groups {
            for (name, value) in &group.preset {
                let field = module.field(name).ok_or_else(|| {
                    format!("the {} group resets an undeclared {name}", group.label)
                })?;
                ensure(
                    value.as_f64() == Some(field.default),
                    format!("the {} group resets {name} to {value}", group.label),
                )?;
            }
        }
        evidence.record(
            "module.list serves the registry's own descriptor, available; schema.list carries the patch with every field optional in declared order and the reset with none, and the mask target exactly when the effect is maskable",
            json!({"set": set, "reset": reset, "fields": names, "maskable": module.effect.maskable, "groups": module.groups.iter().map(|group| group.label.clone()).collect::<Vec<_>>()}),
        );
        Ok(())
    })?;

    let imported = owner.import(editor, fixture)?;
    let asset = imported["asset"]["id"].clone();
    let original = imported["current_entry"]["id"].clone();
    ensure(
        owner.samples(editor, &asset, &source_probes, None)? == source_pixels,
        "the imported Original does not render the decoded source",
    )?;

    within("a neutral first set", || {
        let quiet = Quiet::capture(&owner, editor, &asset)?;
        let mut sent = Vec::new();
        let neutral_by_rule = module
            .single_field_payloads()
            .into_iter()
            .filter(|(_, payload)| registry.layer_neutral(&pixels::layer(module, payload)))
            .map(|(field, payload)| {
                (
                    format!("{} alone, neutral by the module's rule", field.name),
                    payload,
                )
            });
        for (what, payload) in module.neutral_payloads().into_iter().chain(neutral_by_rule) {
            let result = edit(&owner, editor, &asset, set, &payload, None)?;
            no_op(&result, &what)?;
            quiet.unchanged(&owner, editor, &asset, &what)?;
            sent.push(payload);
        }
        let result = edit(&owner, editor, &asset, reset, &json!({}), None)?;
        no_op(&result, "a reset without a layer")?;
        quiet.unchanged(&owner, editor, &asset, "a reset without a layer")?;
        evidence.record(
            "a first set that is neutral, however it is spelled, and a reset without a layer each commit nothing: no layer, entry, revision or event",
            json!({"payloads": sent}),
        );
        Ok(())
    })?;

    within("request refusals", || {
        let quiet = Quiet::capture(&owner, editor, &asset)?;
        let mut unknown = json!({lead.name.clone(): lead.high()});
        unknown["conformance-unknown"] = json!(1.0);
        let mut refused = Vec::new();
        for (what, fields, named) in [
            ("an unknown field", unknown, "conformance-unknown"),
            (
                "a value outside its range",
                json!({lead.name.clone(): lead.outside()}),
                lead.name.as_str(),
            ),
        ] {
            let mut params = fields.clone();
            params["asset_id"] = asset.clone();
            params["mutation"] = mutation(quiet.revision, "refused");
            let (code, message) = owner.refused(editor, &format!("edit.{set}"), params)?;
            ensure(
                code == "validation" && message.contains(named),
                format!("{what} was refused with {code}: {message}"),
            )?;
            quiet.unchanged(&owner, editor, &asset, what)?;
            refused.push(json!({"fields": fields, "code": code, "message": message}));
        }
        if !module.effect.maskable {
            let fields = json!({lead.name.clone(): lead.high()});
            let mut params = fields.clone();
            params["asset_id"] = asset.clone();
            params["mutation"] = mutation(quiet.revision, "refused");
            params["mask"] = json!("mask-conformance");
            let (code, message) = owner.refused(editor, &format!("edit.{set}"), params)?;
            ensure(
                code == "validation",
                format!(
                    "a mask target on an effect that is not maskable was refused with {code}: {message}"
                ),
            )?;
            quiet.unchanged(
                &owner,
                editor,
                &asset,
                "a mask target on an effect that is not maskable",
            )?;
            refused.push(json!({"fields": fields, "mask": "mask-conformance", "code": code, "message": message}));
        }
        evidence.record(
            "a request naming an unknown field, a value outside its range or, for an effect that is not maskable, a mask target is refused and writes nothing",
            json!(refused),
        );
        Ok(())
    })?;

    within("draft begin, set and cancel", || {
        let quiet = Quiet::capture(&owner, editor, &asset)?;
        let begun = owner.call(
            editor,
            "draft.begin",
            json!({"asset_id": asset, "action": set}),
        )?;
        ensure(
            begun["fields"] == json!({})
                && begun["conflicted"] == json!(false)
                && begun["draft_revision"] == json!(0)
                && begun["base_revision"] == json!(quiet.revision),
            format!("draft.begin answered {begun}"),
        )?;
        let draft = begun["draft_id"].clone();
        let moved = owner.call(
            editor,
            "draft.set",
            json!({"draft_id": draft, "fields": {lead.name.clone(): lead.high()}}),
        )?;
        ensure(
            moved["draft_revision"] == json!(1),
            format!("draft.set answered {moved}"),
        )?;
        let (code, _) = owner.refused(
            editor,
            "draft.set",
            json!({"draft_id": draft, "fields": {lead.name.clone(): lead.outside()}}),
        )?;
        ensure(
            code == "validation",
            format!("an out-of-range draft field was refused with {code}"),
        )?;
        let read = owner.call(editor, "draft.read", json!({"draft_id": draft}))?;
        ensure(
            read["fields"] == json!({lead.name.clone(): lead.high()})
                && read["draft_revision"] == json!(1),
            format!("a refused draft.set changed the draft: {read}"),
        )?;
        let (x, y) = source_probes[0];
        let drafted = owner.call(
            editor,
            "render.sample",
            json!({"asset_id": asset, "x": x, "y": y, "draft_id": draft}),
        )?;
        ensure(
            drafted["draft"] == json!({"draft_id": draft, "draft_revision": 1}),
            format!("a drafted sample is stamped {}", drafted["draft"]),
        )?;
        let cancelled = owner.call(editor, "draft.cancel", json!({"draft_id": draft}))?;
        ensure(
            cancelled["cancelled"] == json!(true),
            format!("draft.cancel answered {cancelled}"),
        )?;
        let session = owner.call(editor, "session.state", json!({}))?;
        ensure(
            session["draft"].is_null(),
            format!("draft.cancel left the draft open: {}", session["draft"]),
        )?;
        let (code, _) = owner.refused(editor, "draft.read", json!({"draft_id": draft}))?;
        ensure(
            code == "validation",
            format!("reading a cancelled draft was refused with {code}"),
        )?;
        quiet.unchanged(&owner, editor, &asset, "a cancelled draft")?;
        evidence.record(
            "draft.begin, draft.set and draft.cancel leave nothing: no entry, revision, event or open draft, and an out-of-range draft field is refused without changing the draft",
            json!({"draft": draft, "fields": read["fields"]}),
        );
        Ok(())
    })?;

    let global = within("a gesture commits once", || {
        let before = Quiet::capture(&owner, editor, &asset)?;
        let begun = owner.call(
            editor,
            "draft.begin",
            json!({"asset_id": asset, "action": set}),
        )?;
        let draft = begun["draft_id"].clone();
        for value in [lead.low(), lead.high()] {
            owner.call(
                editor,
                "draft.set",
                json!({"draft_id": draft, "fields": {lead.name.clone(): value}}),
            )?;
        }
        let drafted = owner.samples(editor, &asset, &source_probes, Some(&draft))?;
        ensure(
            drafted != source_pixels,
            "the open draft samples the committed stack instead of the drafted one",
        )?;
        ensure(
            owner.samples(editor, &asset, &source_probes, None)? == source_pixels,
            "an open draft changed what the committed stack samples",
        )?;
        let committed = owner.call(
            editor,
            "draft.commit",
            json!({"draft_id": draft, "mutation": mutation(before.revision, "gesture")}),
        )?;
        applied(&committed, "the gesture's commit")?;
        ensure(
            committed["revision"] == json!(before.revision + 1)
                && owner.head(editor, &asset)? == before.head + 1,
            "the gesture wrote more or fewer than one entry",
        )?;
        ensure(
            owner.call(editor, "session.state", json!({}))?["draft"].is_null(),
            "the committed draft stayed open",
        )?;
        let sent = patch(&[(&lead, lead.high())]);
        let label = expect_entry(&owner, editor, &asset, module, &committed, set, &sent)?;
        ensure(
            owner.samples(editor, &asset, &source_probes, None)? == drafted,
            "the committed stack does not render what its draft previewed",
        )?;
        let found = rows(&owner, editor, &asset, module)?;
        let layer = row(&found, None)?;
        expect_values(module, layer, &sent, "the committed layer")?;
        ensure(
            layer["module"] == json!(module.id)
                && layer["title"] == json!(module.title)
                && layer["available"] == json!(true),
            format!("the committed layer's row reads {layer}"),
        )?;
        let stored = owner.recipe(editor, &asset)?;
        let payload = stored
            .layers
            .iter()
            .find(|layer| layer.effect_id == module.effect.id)
            .map(|layer| layer.payload.clone())
            .ok_or("the committed stack holds no layer of the effect")?;
        ensure(
            payload == Value::Object(sent.clone()),
            format!("the stored payload is {payload}, not the canonical {sent:?}"),
        )?;
        evidence.record(
            "a draft.begin/set/set/commit gesture on the first field that moves a probe pixel on its own previews the change without committing it, commits exactly one entry labelled by the declared rule, storing the patch as sent and the canonical payload, and renders exactly what the draft previewed",
            json!({"field": lead.name, "label": label, "layer": layer["id"], "payload": payload}),
        );
        Ok(layer["id"].clone())
    })?;

    within("a gesture that returns to its start", || {
        let quiet = Quiet::capture(&owner, editor, &asset)?;
        let draft = owner.call(
            editor,
            "draft.begin",
            json!({"asset_id": asset, "action": set}),
        )?["draft_id"]
            .clone();
        for value in [lead.low(), lead.high()] {
            owner.call(
                editor,
                "draft.set",
                json!({"draft_id": draft, "fields": {lead.name.clone(): value}}),
            )?;
        }
        let result = owner.call(
            editor,
            "draft.commit",
            json!({"draft_id": draft, "mutation": mutation(quiet.revision, "return")}),
        )?;
        no_op(&result, "a gesture that returned to its start")?;
        quiet.unchanged(
            &owner,
            editor,
            &asset,
            "a gesture that returned to its start",
        )?;
        let mut spelled = Value::Object(module.defaults());
        spelled[lead.name.as_str()] = json!(lead.high());
        let result = edit(&owner, editor, &asset, set, &spelled, None)?;
        no_op(&result, "a set equal to the stored values")?;
        quiet.unchanged(&owner, editor, &asset, "a set equal to the stored values")?;
        evidence.record(
            "a gesture that returns to its start and a set of the stored values spelled out in full are no-ops: no entry, revision or event",
            json!({"spelled": spelled}),
        );
        Ok(())
    })?;

    within("a retried request", || {
        let revision = owner.revision(editor, &asset)?;
        let mut params = json!({second.name.clone(): second.low()});
        params["asset_id"] = asset.clone();
        params["mutation"] = mutation(revision, "retry");
        let method = format!("edit.{set}");
        let first = owner.call(editor, &method, params.clone())?;
        applied(&first, "the first attempt")?;
        let (head, events) = (owner.head(editor, &asset)?, owner.events(editor)?);
        let retried = owner.call(editor, &method, params.clone())?;
        ensure(
            retried["deduplicated"] == json!(true)
                && retried["current_entry_id"] == first["current_entry_id"]
                && retried["created_entry_id"] == first["created_entry_id"]
                && retried["revision"] == first["revision"],
            format!("the retry answered {retried} against the first {first}"),
        )?;
        ensure(
            owner.head(editor, &asset)? == head && owner.events(editor)? == events,
            "the retry wrote an entry or announced an event",
        )?;
        let mut changed = params.clone();
        changed[second.name.as_str()] = json!(second.default);
        let (code, message) = owner.refused(editor, &method, changed)?;
        ensure(
            code == "conflict",
            format!("the same request id with other input was refused with {code}: {message}"),
        )?;
        evidence.record(
            "a retried request answers the first result marked deduplicated with no entry or event, and the same request id with other input is a conflict",
            json!({"first": first, "retried": retried}),
        );
        Ok(())
    })?;

    within("a whole patch updates the layer in place", || {
        let full = object(&module.full_high())?;
        let result = edit(
            &owner,
            editor,
            &asset,
            set,
            &Value::Object(full.clone()),
            None,
        )?;
        applied(&result, "the whole patch")?;
        let label = expect_entry(&owner, editor, &asset, module, &result, set, &full)?;
        let found = rows(&owner, editor, &asset, module)?;
        let layer = row(&found, None)?;
        ensure(layer["id"] == global, "the whole patch replaced the layer")?;
        expect_values(module, layer, &full, "the whole patch")?;
        ensure(
            layer["neutral"] == json!(false),
            "a moved layer is reported neutral",
        )?;
        let summary = layer["summary"].as_str().unwrap_or_default();
        for field in &module.fields {
            ensure(
                summary.contains(&field.shown(field.high())),
                format!("the row summary {summary:?} does not show {}", field.name),
            )?;
        }
        let pixels = served_equals_render(&owner, editor, &asset, &registry, sources)?;
        evidence.record(
            "a patch of every field updates the one layer in place, labelled by the declared rule, with every value and a summary showing each; render.sample through the owner equals the rendered raster, and sample equals render on the byte and linear paths",
            json!({"label": label, "summary": summary, "pixels": pixels}),
        );
        Ok(())
    })?;

    if module.effect.maskable {
        within("one layer per mask target", || {
            let revision = owner.revision(editor, &asset)?;
            let created = owner.call(
                editor,
                "mask.create-radial",
                json!({"asset_id": asset, "mutation": mutation(revision, "mask"), "x": 0.5, "y": 0.5,
                    "radius_x": 0.3, "radius_y": 0.3, "angle": 0.0, "feather": 50.0}),
            )?;
            let mask = created["mask"].clone();
            ensure(
                !mask.is_null(),
                format!("mask.create-radial answered {created}"),
            )?;

            let quiet = Quiet::capture(&owner, editor, &asset)?;
            let draft = owner.call(
                editor,
                "draft.begin",
                json!({"asset_id": asset, "action": set, "mask": mask}),
            )?["draft_id"]
                .clone();
            owner.call(
                editor,
                "draft.set",
                json!({"draft_id": draft, "fields": {lead.name.clone(): lead.low()}}),
            )?;
            owner.call(editor, "draft.cancel", json!({"draft_id": draft}))?;
            ensure(
                owner.call(editor, "session.state", json!({}))?["draft"].is_null(),
                "draft.cancel left the masked draft open",
            )?;
            quiet.unchanged(&owner, editor, &asset, "a cancelled masked draft")?;

            let global_before = row(&rows(&owner, editor, &asset, module)?, None)?.clone();
            let result = edit(
                &owner,
                editor,
                &asset,
                set,
                &json!({lead.name.clone(): lead.high()}),
                Some(&mask),
            )?;
            applied(&result, "the first masked set")?;
            let found = rows(&owner, editor, &asset, module)?;
            ensure(
                found.len() == 2,
                format!("a masked set left {} layers of the effect", found.len()),
            )?;
            ensure(
                row(&found, None)? == &global_before,
                "the first masked set changed the global layer",
            )?;
            let masked = row(&found, Some(&mask))?.clone();
            expect_values(
                module,
                &masked,
                &patch(&[(&lead, lead.high())]),
                "the masked layer",
            )?;

            let result = edit(
                &owner,
                editor,
                &asset,
                set,
                &json!({second.name.clone(): second.low()}),
                Some(&mask),
            )?;
            applied(&result, "a second masked set")?;
            let found = rows(&owner, editor, &asset, module)?;
            let updated = row(&found, Some(&mask))?;
            ensure(
                updated["id"] == masked["id"],
                "a second masked set replaced the masked layer",
            )?;
            let mut masked_values = patch(&[(&lead, lead.high())]);
            masked_values.insert(second.name.clone(), json!(second.low()));
            expect_values(module, updated, &masked_values, "the updated masked layer")?;
            ensure(
                row(&found, None)? == &global_before,
                "a masked set changed the global layer",
            )?;

            let result = edit(
                &owner,
                editor,
                &asset,
                set,
                &json!({lead.name.clone(): lead.low()}),
                None,
            )?;
            applied(&result, "a global set beside the mask")?;
            let found = rows(&owner, editor, &asset, module)?;
            let global_after = row(&found, None)?;
            ensure(
                global_after["id"] == global,
                "a global set replaced the global layer",
            )?;
            ensure(
                global_after["values"][lead.name.as_str()].as_f64() == Some(lead.low()),
                format!("the global set left {}", global_after["values"]),
            )?;
            expect_values(
                module,
                row(&found, Some(&mask))?,
                &masked_values,
                "the masked layer after a global set",
            )?;

            let pixels = served_equals_render(&owner, editor, &asset, &registry, sources)?;

            let mut ambiguous = owner.recipe(editor, &asset)?;
            let index = ambiguous
                .layers
                .iter()
                .position(|layer| layer.effect_id == module.effect.id && layer.mask.is_some())
                .ok_or("the stack holds no masked layer of the effect")?;
            let mut twin = ambiguous.layers[index].clone();
            twin.id = LayerId::new();
            ambiguous.layers.insert(index + 1, twin);
            let refusal = pixels::refuses_as_ambiguous(&registry, module, sources, &ambiguous)?;

            let result = edit(&owner, editor, &asset, reset, &json!({}), Some(&mask))?;
            applied(&result, "the masked reset")?;
            let found = rows(&owner, editor, &asset, module)?;
            let reset_row = row(&found, Some(&mask))?;
            ensure(
                reset_row["id"] == masked["id"],
                "the masked reset replaced the masked layer",
            )?;
            expect_values(module, reset_row, &Map::new(), "the reset masked layer")?;
            ensure(
                reset_row["neutral"] == json!(true),
                "the reset masked layer is not reported neutral",
            )?;
            ensure(
                row(&found, None)?["values"] == global_after["values"],
                "the masked reset changed the global layer",
            )?;
            let result = edit(&owner, editor, &asset, reset, &json!({}), Some(&mask))?;
            no_op(&result, "a second masked reset")?;
            evidence.record(
                "the global layer and a mask are distinct targets: a masked set commits one layer for its mask and later ones update it, a global set updates only the global layer, a cancelled masked draft leaves nothing, two layers for one mask are refused by name, and the masked reset keeps its layer's identity",
                json!({"mask": mask, "global": global, "masked": masked["id"], "pixels": pixels, "ambiguous": refusal}),
            );
            Ok(())
        })?;
    }

    within("resets keep the layer's identity", || {
        moved_to(
            &owner,
            editor,
            &asset,
            module,
            &module.full_high(),
            "before the resets",
        )?;
        let mut labels = Vec::new();
        for group in &module.groups {
            let before = values_of(row(&rows(&owner, editor, &asset, module)?, None)?)?;
            let preset = Value::Object(group.preset.clone());
            let result = edit(&owner, editor, &asset, set, &preset, None)?;
            applied(&result, &format!("the {} group reset", group.label))?;
            labels.push(expect_entry(
                &owner,
                editor,
                &asset,
                module,
                &result,
                set,
                &group.preset,
            )?);
            let found = rows(&owner, editor, &asset, module)?;
            let layer = row(&found, None)?;
            ensure(
                layer["id"] == global,
                format!("the {} group reset replaced the layer", group.label),
            )?;
            let mut expected = before.clone();
            for (name, value) in &group.preset {
                expected.insert(name.clone(), value.clone());
            }
            let moved: Map<String, Value> = expected
                .into_iter()
                .filter(|(name, value)| {
                    module
                        .field(name)
                        .is_some_and(|field| value.as_f64() != Some(field.default))
                })
                .collect();
            expect_values(
                module,
                layer,
                &moved,
                &format!("after the {} group reset", group.label),
            )?;
            let stored = owner
                .recipe(editor, &asset)?
                .layers
                .into_iter()
                .find(|layer| layer.effect_id == module.effect.id && layer.mask.is_none())
                .map(|layer| layer.payload)
                .ok_or("the global layer is gone")?;
            ensure(
                group.preset.keys().all(|name| stored.get(name).is_none()),
                format!(
                    "the {} group reset left its fields in the stored payload {stored}",
                    group.label
                ),
            )?;
            let again = edit(&owner, editor, &asset, set, &preset, None)?;
            no_op(&again, &format!("a second {} group reset", group.label))?;
        }

        moved_to(
            &owner,
            editor,
            &asset,
            module,
            &module.full_high(),
            "before the module reset",
        )?;
        let result = edit(&owner, editor, &asset, reset, &json!({}), None)?;
        applied(&result, "the module reset")?;
        labels.push(expect_entry(
            &owner,
            editor,
            &asset,
            module,
            &result,
            reset,
            &Map::new(),
        )?);
        let found = rows(&owner, editor, &asset, module)?;
        let layer = row(&found, None)?;
        ensure(layer["id"] == global, "the module reset replaced the layer")?;
        expect_values(module, layer, &Map::new(), "after the module reset")?;
        ensure(
            layer["neutral"] == json!(true) && layer["summary"] == json!("Neutral"),
            format!("the reset layer is described as {layer}"),
        )?;
        let recipe = owner.recipe(editor, &asset)?;
        ensure(
            recipe
                .layers
                .iter()
                .filter(|layer| layer.effect_id == module.effect.id)
                .all(|layer| layer.payload == json!({})),
            "the reset layers do not store the canonical empty payload",
        )?;
        ensure(
            owner.samples(editor, &asset, &source_probes, None)? == source_pixels,
            "a stack of neutral layers does not render the source pixels",
        )?;
        let rendered = pixels::raster(&registry, sources, &recipe)?;
        ensure(
            std::sync::Arc::ptr_eq(&rendered.rgba, &sources.byte.rgba),
            "a stack of neutral layers does not share the source allocation",
        )?;
        let quiet = Quiet::capture(&owner, editor, &asset)?;
        let again = edit(&owner, editor, &asset, reset, &json!({}), None)?;
        no_op(&again, "a second module reset")?;
        quiet.unchanged(&owner, editor, &asset, "a second module reset")?;
        evidence.record(
            "each group reset and the module reset keep the layer's identity, are labelled Reset <group> and Reset <module>, touch only their own fields and are no-ops when repeated; the reset layer stores {} and renders the shared source allocation",
            json!({"layer": global, "labels": labels}),
        );
        Ok(())
    })?;

    within("undo, redo, preview and restore", || {
        let x = edit(&owner, editor, &asset, set, &module.full_high(), None)?;
        applied(&x, "commit X")?;
        let recipe_x = owner.recipe(editor, &asset)?;
        let y = edit(&owner, editor, &asset, set, &module.full_low(), None)?;
        applied(&y, "commit Y")?;
        let recipe_y = owner.recipe(editor, &asset)?;
        let pixels_of = |recipe: &Recipe| -> Checked<Vec<Value>> {
            let raster = pixels::raster(&registry, sources, recipe)?;
            Ok(source_probes
                .iter()
                .map(|&(x, y)| json!(raster.pixel(x, y)))
                .collect())
        };
        let (pixels_x, pixels_y) = (pixels_of(&recipe_x)?, pixels_of(&recipe_y)?);
        ensure(
            pixels_x != pixels_y,
            "the two commits render the same probes, so history could not be told apart",
        )?;
        let history = |method: &str, extra: Value| -> Checked<Value> {
            let mut params = extra;
            params["asset_id"] = asset.clone();
            params["mutation"] = mutation(owner.revision(editor, &asset)?, method);
            owner.call(editor, method, params)
        };
        let undone = history("history.undo", json!({}))?;
        ensure(
            undone["outcome"] != json!("no-op"),
            format!("undo answered {undone}"),
        )?;
        expect_current(
            &owner,
            editor,
            &asset,
            &recipe_x,
            &pixels_x,
            &source_probes,
            "after undo",
        )?;
        history("history.redo", json!({}))?;
        expect_current(
            &owner,
            editor,
            &asset,
            &recipe_y,
            &pixels_y,
            &source_probes,
            "after redo",
        )?;
        let revision = owner.revision(editor, &asset)?;
        owner.call(
            editor,
            "preview.select",
            json!({"asset_id": asset, "entry_id": x["current_entry_id"]}),
        )?;
        ensure(
            owner.revision(editor, &asset)? == revision,
            "previewing an entry committed something",
        )?;
        ensure(
            owner.samples(editor, &asset, &source_probes, None)? == pixels_x,
            "the preview of X does not render X",
        )?;
        owner.call(editor, "preview.return-current", json!({}))?;
        ensure(
            owner.samples(editor, &asset, &source_probes, None)? == pixels_y,
            "returning to current does not render Y",
        )?;
        history(
            "history.restore",
            json!({"entry_id": x["current_entry_id"]}),
        )?;
        expect_current(
            &owner,
            editor,
            &asset,
            &recipe_x,
            &pixels_x,
            &source_probes,
            "after restoring X",
        )?;
        history(
            "history.restore",
            json!({"entry_id": y["current_entry_id"]}),
        )?;
        expect_current(
            &owner,
            editor,
            &asset,
            &recipe_y,
            &pixels_y,
            &source_probes,
            "after restoring Y",
        )?;
        evidence.record(
            "undo, redo, a read-only preview and restore each return exactly the stack of the entry they name, and render.sample answers that stack's rendered pixels",
            json!({"x": x["current_entry_id"], "y": y["current_entry_id"], "pixels_x": pixels_x, "pixels_y": pixels_y}),
        );
        Ok(())
    })?;

    within("sample equals render through a crop", || {
        let result = edit(
            &owner,
            editor,
            &asset,
            "crop",
            &json!({"angle": 0.0, "x": 0.1, "y": 0.1, "width": 0.6, "height": 0.6}),
            None,
        )?;
        applied(&result, "an axis-aligned crop")?;
        let exact = served_equals_render(&owner, editor, &asset, &registry, sources)?;
        let result = edit(
            &owner,
            editor,
            &asset,
            "crop-fit",
            &json!({"aspect": "16:9", "angle": 8.0}),
            None,
        )?;
        applied(&result, "a straightened crop")?;
        let recipe = owner.recipe(editor, &asset)?;
        ensure(
            recipe
                .layers
                .iter()
                .any(|layer| layer.payload["angle"].as_f64() == Some(8.0)),
            "the straightened crop is not in the stack",
        )?;
        let straightened = served_equals_render(&owner, editor, &asset, &registry, sources)?;
        evidence.record(
            "through an axis-aligned crop and a straightened, resampling crop, render.sample equals the rendered raster, and sample equals render on the byte and linear paths",
            json!({"exact": exact, "straightened": straightened}),
        );
        Ok(())
    })?;

    within("two clients", || {
        let revision = owner.revision(editor, &asset)?;
        let current = values_of(row(&rows(&owner, editor, &asset, module)?, None)?)?;
        let value = |field: &Field| {
            other_than(
                field,
                current
                    .get(&field.name)
                    .and_then(Value::as_f64)
                    .unwrap_or(field.default),
            )
        };
        let (mine, theirs) = (value(&lead), value(&second));
        let draft = owner.call(
            editor,
            "draft.begin",
            json!({"asset_id": asset, "action": set}),
        )?["draft_id"]
            .clone();
        owner.call(
            editor,
            "draft.set",
            json!({"draft_id": draft, "fields": {lead.name.clone(): mine}}),
        )?;
        let agent_commit = edit(
            &owner,
            agent,
            &asset,
            set,
            &json!({second.name.clone(): theirs}),
            None,
        )?;
        applied(&agent_commit, "the agent's commit")?;
        ensure(
            owner.call(agent, "session.state", json!({}))?["draft"].is_null(),
            "the agent acquired the editor's draft",
        )?;
        let read = owner.call(editor, "draft.read", json!({"draft_id": draft}))?;
        ensure(
            read["conflicted"] == json!(true) && read["fields"] == json!({lead.name.clone(): mine}),
            format!("the drafted gesture answered {read}"),
        )?;
        let (code, _) = owner.refused(
            editor,
            "draft.commit",
            json!({"draft_id": draft, "mutation": mutation(revision, "stale")}),
        )?;
        ensure(
            code == "conflict",
            format!("a conflicted commit was refused with {code}"),
        )?;
        let reapplied = owner.call(editor, "draft.reapply", json!({"draft_id": draft}))?;
        let rebased = owner.revision(editor, &asset)?;
        ensure(
            reapplied["conflicted"] == json!(false)
                && reapplied["base_revision"] == json!(rebased)
                && reapplied["fields"] == json!({lead.name.clone(): mine}),
            format!("reapply answered {reapplied}"),
        )?;
        let head = owner.head(editor, &asset)?;
        let committed = owner.call(
            editor,
            "draft.commit",
            json!({"draft_id": draft, "mutation": mutation(rebased, "rebased")}),
        )?;
        applied(&committed, "the reapplied commit")?;
        ensure(
            committed["revision"] == json!(rebased + 1) && owner.head(editor, &asset)? == head + 1,
            "the reapplied commit wrote more or fewer than one entry",
        )?;
        let after = values_of(row(&rows(&owner, editor, &asset, module)?, None)?)?;
        ensure(
            after.get(&lead.name).and_then(Value::as_f64) == Some(mine)
                && (second.name == lead.name
                    || after.get(&second.name).and_then(Value::as_f64) == Some(theirs)),
            format!("reapply lost a field: {after:?}"),
        )?;

        owner.call(
            editor,
            "preview.select",
            json!({"asset_id": asset, "entry_id": original}),
        )?;
        let later = edit(
            &owner,
            agent,
            &asset,
            set,
            &json!({lead.name.clone(): other_than(&lead, mine)}),
            None,
        )?;
        applied(&later, "the agent's commit during a preview")?;
        let session = owner.call(editor, "session.state", json!({}))?;
        ensure(
            session["preview"]["selection"] == json!({"entry": original}),
            format!("the selection moved to {}", session["preview"]["selection"]),
        )?;
        ensure(
            owner.samples(editor, &asset, &source_probes, None)? == source_pixels,
            "the previewed Original moved to the newest stack",
        )?;
        owner.call(editor, "preview.return-current", json!({}))?;
        evidence.record(
            "while the editor drafts one field the agent commits another: the draft reads conflicted, its commit is refused, reapply rebases it and commits exactly one entry keeping both fields; a historical selection stays on its entry through the agent's next commit",
            json!({"reapplied": reapplied, "committed": committed, "values": after}),
        );
        Ok(())
    })?;

    let state = within("the final state", || {
        let state = owner.state(editor, &asset)?;
        let recipe = owner.recipe(editor, &asset)?;
        let raster = pixels::raster(&registry, sources, &recipe)?;
        let probes = pixels::probes(raster.width, raster.height);
        let analysis = owner.analyse(editor, &asset, json!({"kind": "current"}))?;
        ensure(
            analysis["status"] == json!("ready"),
            format!(
                "the final stack's analysis settled as {}",
                analysis["status"]
            ),
        )?;
        Ok(Final {
            revision: owner.revision(editor, &asset)?,
            entry: state["current_entry"]["id"].clone(),
            rows: owner.describe(editor, &asset)?["layers"].clone(),
            samples: owner.samples(editor, &asset, &probes, None)?,
            probes,
            recipe,
            identity: analysis["identity"].clone(),
            asset: asset.clone(),
        })
    })?;
    owner.close()?;
    Ok(state)
}

/// The same catalog served with the module registered unavailable: its layers stay stored and
/// readable, and rendering, sampling, analysis and a new edit are each refused by name.
pub fn unavailable(module: &FieldPatch, catalog: &Path, state: &Final) -> Checked<Value> {
    let owner = Owner::start(catalog, registry_without(&module.id)?, ACTOR)?;
    let client = owner.client();
    let asset = &state.asset;
    owner.prepare(client, asset)?;
    let listed = owner.call(client, "module.list", json!({}))?;
    let served = listed["modules"]
        .as_array()
        .ok_or("module.list answered no modules")?
        .iter()
        .find(|served| served["id"] == json!(module.id))
        .cloned()
        .ok_or("an unavailable module is not listed at all")?;
    ensure(
        served["availability"]["kind"] == json!("unavailable"),
        format!(
            "the disabled module reports availability {}",
            served["availability"]
        ),
    )?;
    let recipe = owner.recipe(client, asset)?;
    ensure(
        recipe.layers == state.recipe.layers && recipe.masks == state.recipe.masks,
        "serving the catalog without the module changed the stored stack",
    )?;
    let described = owner.describe(client, asset)?;
    let unavailable_rows: Vec<&Value> = described["layers"]
        .as_array()
        .ok_or("recipe.describe answered no layers")?
        .iter()
        .filter(|row| row["effect"] == json!(module.effect.id))
        .collect();
    ensure(
        !unavailable_rows.is_empty()
            && unavailable_rows.iter().all(|row| {
                row["available"] == json!(false)
                    && row["summary"]
                        .as_str()
                        .is_some_and(|summary| summary.starts_with("unavailable"))
            }),
        format!("the layers of an unavailable effect read {unavailable_rows:?}"),
    )?;
    let (code, message) = owner.refused(
        client,
        "render.sample",
        json!({"asset_id": asset, "x": 0, "y": 0}),
    )?;
    ensure(
        code == "incompatible" && message.contains(&module.effect.id),
        format!("sampling a stack of an unavailable effect was refused with {code}: {message}"),
    )?;
    let lead = &module.fields[0];
    let mut params = json!({lead.name.clone(): lead.high()});
    params["asset_id"] = asset.clone();
    params["mutation"] = mutation(state.revision, "unavailable");
    let (edit_code, edit_message) =
        owner.refused(client, &format!("edit.{}", module.set), params)?;
    ensure(
        edit_code == "incompatible" && edit_message.contains(&module.id),
        format!(
            "an edit through an unavailable module was refused with {edit_code}: {edit_message}"
        ),
    )?;
    ensure(
        owner.revision(client, asset)? == state.revision,
        "a refused edit moved the revision",
    )?;
    let analysis = owner.analyse(client, asset, json!({"kind": "current"}))?;
    ensure(
        analysis["status"] == json!("failed") && analysis.get("report").is_none(),
        format!("the analysis of a stack of an unavailable effect answered {analysis}"),
    )?;
    let history = owner.call(
        client,
        "history.list",
        json!({"asset_id": asset, "limit": 5}),
    )?;
    ensure(
        history["entries"]
            .as_array()
            .is_some_and(|entries| !entries.is_empty()),
        "history is unreadable while the module is unavailable",
    )?;
    owner.close()?;
    Ok(json!({
        "rows": unavailable_rows,
        "render": message,
        "edit": edit_message,
        "analysis": analysis["status"],
    }))
}

/// The catalog reopened with every module available returns the same revision, entry, layer and
/// mask identities, rows and pixels, and the same analysis identity.
pub fn reopen(catalog: &Path, state: &Final) -> Checked<Value> {
    let owner = Owner::start(catalog, ModuleRegistry::builtin(), ACTOR)?;
    let client = owner.client();
    let asset = &state.asset;
    owner.prepare(client, asset)?;
    let current = owner.state(client, asset)?;
    ensure(
        owner.revision(client, asset)? == state.revision
            && current["current_entry"]["id"] == state.entry,
        format!(
            "the reopened catalog is at revision {} entry {}",
            current["revision"], current["current_entry"]["id"]
        ),
    )?;
    let recipe = owner.recipe(client, asset)?;
    ensure(
        recipe.layers == state.recipe.layers && recipe.masks == state.recipe.masks,
        "the reopened stack differs in its layers or masks",
    )?;
    ensure(
        owner.describe(client, asset)?["layers"] == state.rows,
        "the reopened rows differ",
    )?;
    ensure(
        owner.samples(client, asset, &state.probes, None)? == state.samples,
        "the reopened stack renders other pixels",
    )?;
    let analysis = owner.analyse(client, asset, json!({"kind": "current"}))?;
    ensure(
        analysis["status"] == json!("ready") && analysis["identity"] == state.identity,
        format!(
            "the reopened analysis answered {} with identity {}, not {}",
            analysis["status"], analysis["identity"], state.identity
        ),
    )?;
    owner.close()?;
    Ok(json!({
        "revision": state.revision,
        "entry": state.entry,
        "layers": recipe.layers.iter().map(|layer| json!({"id": layer.id, "effect": layer.effect_id, "mask": layer.mask})).collect::<Vec<_>>(),
        "identity": state.identity,
    }))
}
