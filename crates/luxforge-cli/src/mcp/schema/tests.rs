//! The generated schemas against the 2020-12 metaschema and the registries they come from, each
//! kind's mapping, the envelope's rules, and the adapter's own check against an independent JSON
//! Schema validator, `jsonschema`.
use super::*;
use luxforge_core::{
    AssetId, ComponentId, CorrectionId, DraftId, EntryId, JobId, MaskId, ModuleRegistry, PresetId,
    schemas,
};
use serde_json::json;

const PATHS: [f64; 2] = [-1.0, 2.0];

/// The compiled validator of a generated schema, which must be a valid 2020-12 schema first.
fn validator(schema: &Value) -> jsonschema::Validator {
    if let Err(error) = jsonschema::draft202012::meta::validate(schema) {
        panic!("not a 2020-12 schema: {error}\n{schema:#}");
    }
    jsonschema::draft202012::new(schema).expect("a generated schema compiles")
}

/// A host method's entry declaring `parameters`, with `required` listing the required ones, as
/// `schema.list` lists a method with no envelope.
fn host_entry(parameters: &[ParameterDescriptor]) -> Value {
    let required: Vec<&str> = parameters
        .iter()
        .filter(|parameter| parameter.required)
        .map(|parameter| parameter.name.as_str())
        .collect();
    let optional: Map<String, Value> = parameters
        .iter()
        .filter(|parameter| !required.contains(&parameter.name.as_str()))
        .map(|parameter| (parameter.name.clone(), json!(parameter.notes)))
        .collect();
    json!({
        "mutates": false,
        "required": required,
        "optional": optional,
        "notes": "a test method",
        "parameters": parameters,
    })
}

fn input(entry: &Value) -> MethodInput {
    MethodInput::read("test.method", entry, PATHS).expect("the entry reads")
}

/// One required parameter's mapping: its property schema, the values both the adapter's check and
/// the schema accept, and the values the adapter refuses, naming the parameter, with whether the
/// schema refuses each too. The schema never refuses what the adapter accepts.
fn assert_mapping(
    parameter: ParameterDescriptor,
    expected: Value,
    accepted: &[Value],
    refused: &[(Value, &str, bool)],
) {
    let name = parameter.name.clone();
    let method = input(&host_entry(&[parameter.required(true)]));
    let schema = method.input_schema();
    assert_eq!(schema["properties"][&name], expected, "{name}'s mapping");
    assert_eq!(schema["required"], json!([name]));
    let validator = validator(&schema);
    for value in accepted {
        let arguments = json!({ &name: value });
        assert_eq!(
            method.check(&arguments).map(|checked| checked.params),
            Ok(arguments.as_object().cloned().unwrap()),
            "the adapter accepts {value}"
        );
        assert!(validator.is_valid(&arguments), "the schema accepts {value}");
    }
    for (value, message, schema_refuses) in refused {
        let arguments = json!({ &name: value });
        let error = method.check(&arguments).unwrap_err();
        assert_eq!(error.parameter.as_deref(), Some(name.as_str()), "{value}");
        assert!(
            error.message.contains(message) && error.message.contains(&name),
            "{value}: {}",
            error.message
        );
        assert_eq!(
            !validator.is_valid(&arguments),
            *schema_refuses,
            "whether the schema refuses {value}"
        );
    }
}

#[test]
fn integer_maps_to_an_integer_within_its_range() {
    assert_mapping(
        ParameterDescriptor::integer("limit", 1, 100).notes("a page"),
        json!({"type": "integer", "minimum": 1, "maximum": 100, "description": "A page."}),
        &[json!(1), json!(100)],
        &[
            (json!(0), "within 1..=100", true),
            (json!(101), "within 1..=100", true),
            (json!(1.5), "must be an integer", true),
            (json!("8"), "must be an integer", true),
        ],
    );
}

#[test]
fn number_maps_to_a_number_with_its_hints_described() {
    assert_mapping(
        ParameterDescriptor::number("exposure", -5.0, 5.0)
            .notes("exposure.")
            .unit("EV")
            .step(0.01)
            .precision(2)
            .soft_min(-2.0)
            .soft_max(2.0)
            .default(0.0),
        json!({
            "type": "number",
            "minimum": -5.0,
            "maximum": 5.0,
            "description": "Exposure. Unit: EV. Step 0.01. Soft range -2 to 2. Shown to 2 decimal places.",
            "default": 0.0,
        }),
        &[json!(-5.0), json!(1), json!(4.99)],
        &[
            (json!(5.5), "within -5..=5", true),
            (json!(true), "must be a number", true),
        ],
    );
}

#[test]
fn enum_maps_to_its_options() {
    assert_mapping(
        ParameterDescriptor::enumeration("mode", ["heal", "clone"]).default("heal"),
        json!({"type": "string", "enum": ["heal", "clone"], "default": "heal"}),
        &[json!("clone")],
        &[
            (json!("smudge"), "must be one of heal, clone", true),
            (json!(1), "must be a string", true),
        ],
    );
}

#[test]
fn boolean_maps_to_a_boolean() {
    assert_mapping(
        ParameterDescriptor::boolean("erase"),
        json!({"type": "boolean"}),
        &[json!(true), json!(false)],
        &[(json!("true"), "must be a boolean", true)],
    );
}

#[test]
fn string_maps_to_a_string_bounded_in_characters() {
    assert_mapping(
        ParameterDescriptor::new("name", ParameterKind::String { max_length: 4 }),
        json!({"type": "string", "maxLength": 4, "description": "No control characters."}),
        &[json!(""), json!("éééé")],
        &[
            (json!("five!"), "at most 4 characters", true),
            (json!("a\nb"), "control characters", false),
            (json!(4), "must be a string", true),
        ],
    );
}

/// A text bound is in bytes, which `maxLength` cannot state, so the adapter checks it: four
/// two-byte characters fit the schema and are refused.
#[test]
fn text_maps_to_a_string_with_its_byte_bound_checked_by_the_adapter() {
    assert_mapping(
        ParameterDescriptor::new("path", ParameterKind::Text { max_bytes: 6 }),
        json!({
            "type": "string",
            "description": "UTF-8 text of at most 6 bytes, which may hold line breaks.",
        }),
        &[json!("a\nb"), json!("ééé")],
        &[
            (json!("éééé"), "at most 6 bytes", false),
            (json!(["a"]), "must be a string", true),
        ],
    );
}

#[test]
fn identity_maps_to_a_string_described_as_its_kind() {
    assert_mapping(
        ParameterDescriptor::new(
            "draft_id",
            ParameterKind::Identity {
                of: IdentityKind::Draft,
            },
        ),
        json!({"type": "string", "description": "A draft id."}),
        &[json!(DraftId::new())],
        &[
            (json!(AssetId::new()), "must be a draft identity", false),
            (json!(7), "must be a draft identity", true),
        ],
    );
    // Every identity kind names its object.
    for (of, noun) in [
        (IdentityKind::Asset, "An asset id."),
        (IdentityKind::Entry, "An entry id."),
        (IdentityKind::Job, "A job id."),
        (IdentityKind::Preset, "A preset id."),
        (IdentityKind::Mask, "A mask id."),
        (IdentityKind::Component, "A mask component id."),
        (IdentityKind::Stroke, "A stroke (content address) id."),
        (IdentityKind::CatalogFolder, "A catalog folder id."),
        (IdentityKind::Collection, "A collection id."),
        (IdentityKind::Correction, "A correction id."),
    ] {
        let parameter = ParameterDescriptor::new("id", ParameterKind::Identity { of });
        let schema = input(&host_entry(&[parameter])).input_schema();
        assert_eq!(schema["properties"]["id"]["description"], noun);
    }
}

#[test]
fn artifact_maps_to_a_string() {
    assert_mapping(
        ParameterDescriptor::new("artifact", ParameterKind::Artifact),
        json!({
            "type": "string",
            "description": "An artifact id of a derived artifact published in this catalog.",
        }),
        &[json!(format!("artifact-{}", "a".repeat(64)))],
        &[(json!("artifact-1"), "must be an artifact identity", false)],
    );
}

#[test]
fn points_map_to_an_array_of_pairs_within_the_published_path_range() {
    let pair = json!({
        "type": "array",
        "prefixItems": [
            {"type": "number", "minimum": -1.0, "maximum": 2.0},
            {"type": "number", "minimum": -1.0, "maximum": 2.0},
        ],
        "minItems": 2,
        "maxItems": 2,
    });
    assert_mapping(
        ParameterDescriptor::new(
            "box",
            ParameterKind::Points {
                points_min: 2,
                points_max: 2,
            },
        ),
        json!({
            "type": "array",
            "items": pair,
            "minItems": 2,
            "maxItems": 2,
            "description": "A path of [x, y] positions in the content stage's normalized coordinates, in drawn order.",
        }),
        &[json!([[0.1, 0.2], [-1, 2]])],
        &[
            (json!([[0.1, 0.2]]), "at least 2 positions", true),
            (
                json!([[0, 0], [0, 0], [0, 0]]),
                "the limit is 2 positions",
                true,
            ),
            (json!([[0.1, 0.2], [2.5, 0]]), "position 1 must hold", true),
            (json!([[0.1, 0.2], [0.5]]), "malformed position 1", true),
            (json!("path"), "must be a path", true),
        ],
    );
}

#[test]
fn curve_maps_to_an_array_of_unit_pairs_with_its_order_described() {
    assert_mapping(
        ParameterDescriptor::curve("luminance", 2, 3).monotone(),
        json!({
            "type": "array",
            "items": {
                "type": "array",
                "prefixItems": [
                    {"type": "number", "minimum": 0.0, "maximum": 1.0},
                    {"type": "number", "minimum": 0.0, "maximum": 1.0},
                ],
                "minItems": 2,
                "maxItems": 2,
            },
            "minItems": 2,
            "maxItems": 3,
            "description": "Curve points [x, y] within 0..1, x strictly increasing and y never decreasing.",
        }),
        &[json!([[0, 0], [0.5, 0.6], [1, 1]])],
        &[
            (json!([[0, 0]]), "invalid curve point count", true),
            (json!([[0, 0], [1.5, 1]]), "invalid curve point 1", true),
            (
                json!([[0.5, 0.5], [0.4, 0.6]]),
                "invalid curve point 1",
                false,
            ),
            (json!([[0, 0.5], [1, 0.4]]), "invalid curve point 1", false),
        ],
    );
    let fixed = ParameterDescriptor::new(
        "hues",
        ParameterKind::Curve {
            points_min: 2,
            points_max: 2,
            monotone: false,
            fixed_x: Some(vec![0.0, 0.5]),
        },
    );
    let schema = input(&host_entry(&[fixed])).input_schema();
    assert_eq!(
        schema["properties"]["hues"]["description"],
        "Curve points [x, y] within 0..1, x strictly increasing, at exactly the x positions 0, 0.5."
    );
}

#[test]
fn color_maps_to_three_byte_channels() {
    assert_mapping(
        ParameterDescriptor::color("rgb").default(json!([64, 128, 192])),
        json!({
            "type": "array",
            "items": {"type": "integer", "minimum": 0, "maximum": 255},
            "minItems": 3,
            "maxItems": 3,
            "description": "Three 8-bit sRGB channels [r, g, b].",
            "default": [64, 128, 192],
        }),
        &[json!([0, 255, 7])],
        &[
            (json!([0, 256, 7]), "three sRGB channels", true),
            (json!([0, 255]), "three sRGB channels", true),
        ],
    );
}

#[test]
fn settings_map_to_an_object_of_action_field_objects() {
    assert_mapping(
        ParameterDescriptor::new("settings", ParameterKind::Settings),
        json!({
            "type": "object",
            "minProperties": 1,
            "additionalProperties": {"type": "object"},
            "description": "A settings set: field-patch action identities, each with an object of that action's fields.",
        }),
        &[
            json!({"set-basic": {"exposure": 0.5}}),
            json!({"auto-tone": {}}),
        ],
        &[
            (json!({}), "must name 1..=16 actions", true),
            (json!({"set-basic": 1}), "an object of fields", true),
            (
                json!({"Not An Action": {}}),
                "invalid action identity",
                false,
            ),
        ],
    );
}

#[test]
fn settings_origin_maps_to_its_two_wire_forms() {
    let identity = json!({"type": ["string", "null"]});
    let text = json!({"type": "string", "minLength": 1});
    assert_mapping(
        ParameterDescriptor::new("origin", ParameterKind::SettingsOrigin),
        json!({"oneOf": [
            {
                "type": "object",
                "properties": {"kind": {"const": "preset"}, "name": text, "preset_id": identity},
                "required": ["kind", "name"],
                "additionalProperties": false,
            },
            {
                "type": "object",
                "properties": {"kind": {"const": "paste"}, "source": text, "source_asset": identity},
                "required": ["kind", "source"],
                "additionalProperties": false,
            },
        ]}),
        &[
            json!({"kind": "preset", "name": "Warm", "preset_id": PresetId::new()}),
            json!({"kind": "paste", "source": "DSC_0001.NEF"}),
        ],
        &[
            (
                json!({"kind": "paste", "name": "Warm"}),
                "not a usable origin",
                true,
            ),
            (
                json!({"kind": "preset", "name": "Warm", "extra": 1}),
                "not a usable origin",
                true,
            ),
            (
                json!({"kind": "preset", "name": " "}),
                "must not be empty",
                false,
            ),
        ],
    );
}

/// Only a module setting declares an endpoint, so no method lists one; its mapping is still the
/// URL string its setting takes, checked by the core's transport policy.
#[test]
fn endpoint_maps_to_a_url_string() {
    assert_mapping(
        ParameterDescriptor::new(
            "endpoint",
            ParameterKind::Endpoint {
                classes: Vec::new(),
            },
        ),
        json!({"type": "string", "description": "A URL the host's transport policy accepts."}),
        &[],
        &[
            (json!("not a url"), "endpoint", false),
            (json!(1), "at most 4096 bytes", true),
        ],
    );
}

/// A secret is a plain string to the adapter, which the core's value check never takes: it is
/// bounded in characters as its setting stores it.
#[test]
fn secret_maps_to_a_bounded_string() {
    assert_mapping(
        ParameterDescriptor::new("value", ParameterKind::Secret { max_length: 4 }),
        json!({
            "type": "string",
            "minLength": 1,
            "maxLength": 4,
            "description": "A secret, kept only in the operating system's secret store.",
        }),
        &[json!("k"), json!("keys")],
        &[
            (json!(""), "1..=4 characters", true),
            (json!("keys!"), "1..=4 characters", true),
            (json!(4), "1..=4 characters", true),
        ],
    );
}

#[test]
fn json_maps_to_any_value_described_by_its_notes() {
    assert_mapping(
        ParameterDescriptor::new("fields", ParameterKind::Json).notes("{x, y}: a draft's fields"),
        json!({"description": "{x, y}: a draft's fields."}),
        &[json!({"x": 1}), json!([1]), json!("x"), Value::Null],
        &[],
    );
}

/// A parameter kind this build does not know refuses the entry, naming the method and parameter:
/// it is never offered as an empty schema.
#[test]
fn an_unknown_kind_refuses_the_entry() {
    let mut entry = host_entry(&[ParameterDescriptor::boolean("flag")]);
    entry["parameters"][0]["kind"] = json!("hologram");
    let error = MethodInput::read("test.method", &entry, PATHS).unwrap_err();
    assert!(
        error.0.contains("test.method") && error.0.contains("parameter flag"),
        "{error}"
    );
    assert!(error.0.contains("unknown variant `hologram`"), "{error}");
    let schema_list = json!({"paths": {"range": PATHS}, "methods": {"test.method": entry}});
    assert_eq!(MethodInputs::read(&schema_list).unwrap_err(), error);
}

/// An entry that cannot be read as this adapter's protocol is refused by name: an envelope or
/// revision it does not know, a field nothing types, or an answer without the path range.
#[test]
fn an_unreadable_entry_is_refused() {
    let base = host_entry(&[ParameterDescriptor::boolean("flag")]);
    let with = |key: &str, value: Value| {
        let mut entry = base.clone();
        entry[key] = value;
        entry
    };
    for (entry, refusal) in [
        (with("mutation", json!("teleport")), "names the envelope"),
        (with("mutation", json!("revision")), "pairs its envelope"),
        (with("revision_of", json!("asset")), "pairs its envelope"),
        (
            with("revision_of", json!("galaxy")),
            "names the revision of",
        ),
        (with("mutation", json!("request")), "does not require"),
        (
            with("required", json!(["colour"])),
            "lists colour, which no",
        ),
        (
            with("required", json!(["flag", "flag"])),
            "lists flag twice",
        ),
        (with("optional", json!({})), "neither required nor optional"),
        (with("parameters", json!(null)), "lists no parameters"),
    ] {
        let error = MethodInput::read("test.method", &entry, PATHS).unwrap_err();
        assert!(error.0.contains(refusal), "{refusal}: {error}");
    }
    let mut collides = host_entry(&[ParameterDescriptor::boolean(EXPECTED_REVISION)]);
    collides["mutates"] = json!(true);
    assert!(
        MethodInput::read("test.method", &collides, PATHS)
            .unwrap_err()
            .0
            .contains("lists expected_revision twice")
    );
    let error = MethodInputs::read(&json!({"methods": {}})).unwrap_err();
    assert!(error.0.contains("paths.range"), "{error}");
}

/// The fields an entry lists beside its declared parameters are typed as the host takes them, and
/// an optional one is described in the entry's words for it.
#[test]
fn host_fields_are_typed_as_the_host_takes_them() {
    let target = ParameterDescriptor::new(
        "mask",
        ParameterKind::Identity {
            of: IdentityKind::Mask,
        },
    )
    .notes("the mask this edit applies through");
    let entry = json!({
        "mutates": false,
        "required": ["asset_id", "profile_id"],
        "optional": {"entry_id": "entry to ask about", "mask": "the mask whose stack to ask about"},
        "notes": "",
        "parameters": [],
        "target": target,
    });
    let method = MethodInput::read("query.test", &entry, PATHS).unwrap();
    let schema = method.input_schema();
    assert_eq!(
        schema["properties"],
        json!({
            "asset_id": {"type": "string", "description": "An asset id."},
            "profile_id": {"type": "string", "maxLength": 256, "description": "No control characters."},
            "entry_id": {"type": "string", "description": "An entry id. Entry to ask about."},
            "mask": {"type": "string", "description": "A mask id. The mask whose stack to ask about."},
        })
    );
    assert_eq!(schema["required"], json!(["asset_id", "profile_id"]));
    let error = method
        .check(&json!({"asset_id": AssetId::new(), "profile_id": "p", "mask": "entry-x"}))
        .unwrap_err();
    assert_eq!(error.parameter.as_deref(), Some("mask"));
    // A query's fields are its generic check's, which refuses `null`.
    let error = method
        .check(&json!({"asset_id": AssetId::new(), "profile_id": "p", "entry_id": null}))
        .unwrap_err();
    assert_eq!(
        error.message,
        "parameter entry_id must be an entry identity"
    );
}

/// A host method reads an optional `null` as absent, or as clearing a field that can be cleared,
/// so its schema and check accept one; a required field and a generated method's do not.
#[test]
fn null_is_accepted_only_where_the_method_reads_it_as_absent() {
    let parameters = [
        ParameterDescriptor::enumeration("look", ["standard", "neutral"]),
        ParameterDescriptor::integer("size", 100, 150).required(true),
    ];
    let host = input(&host_entry(&parameters));
    let schema = host.input_schema();
    assert_eq!(
        schema["properties"]["look"],
        json!({"type": ["string", "null"], "enum": ["standard", "neutral", null]})
    );
    assert_eq!(
        schema["properties"]["size"],
        json!({"type": "integer", "minimum": 100, "maximum": 150})
    );
    let validator = validator(&schema);
    let reset = json!({"look": null, "size": 100});
    assert!(validator.is_valid(&reset));
    assert_eq!(host.check(&reset).unwrap().params["look"], Value::Null);
    let required_null = json!({"size": null});
    assert!(!validator.is_valid(&required_null));
    assert_eq!(
        host.check(&required_null).unwrap_err().message,
        "parameter size must be an integer"
    );
    // An action's parameters are its module's, whose check refuses null.
    let mut action = host_entry(&parameters);
    action["patch"] = json!(true);
    let action = input(&action);
    let schema = action.input_schema();
    assert_eq!(
        schema["properties"]["look"],
        json!({"type": "string", "enum": ["standard", "neutral"]})
    );
    let error = action.check(&reset).unwrap_err();
    assert_eq!(error.parameter.as_deref(), Some("look"));
    assert!(!jsonschema::is_valid(&schema, &reset));
}

/// The envelope is never a property. A revision of an asset is an optional `expected_revision`, a
/// draft's or a module's settings' a required one, and a method without a revision has none.
#[test]
fn expected_revision_follows_whose_revision_it_is() {
    let base = |mutation: &str, revision_of: Option<&str>| {
        let mut entry = host_entry(&[ParameterDescriptor::boolean("flag")]);
        entry["mutates"] = json!(true);
        entry["mutation"] = json!(mutation);
        entry["required"] = json!(["mutation"]);
        if let Some(of) = revision_of {
            entry["revision_of"] = json!(of);
        }
        input(&entry)
    };
    for (method, expected, required) in [
        (
            base("revision", Some("asset")),
            ExpectedRevision::Optional,
            json!([]),
        ),
        (
            base("revision", Some("draft")),
            ExpectedRevision::Required,
            json!([EXPECTED_REVISION]),
        ),
        (
            base("revision", Some("module-settings")),
            ExpectedRevision::Required,
            json!([EXPECTED_REVISION]),
        ),
        (base("request", None), ExpectedRevision::None, json!([])),
    ] {
        assert_eq!(method.expected_revision(), expected);
        let schema = method.input_schema();
        assert_eq!(schema["required"], required);
        assert!(schema["properties"].get(MUTATION).is_none());
        assert_eq!(schema["additionalProperties"], false);
        let validator = validator(&schema);
        let revision = schema["properties"].get(EXPECTED_REVISION);
        assert_eq!(revision.is_some(), expected != ExpectedRevision::None);
        if let Some(revision) = revision {
            assert_eq!(revision["type"], "integer");
            assert_eq!(revision["minimum"], 0);
        }
        // The adapter builds the envelope, so an agent never passes one.
        let envelope = json!({"mutation": {"request_id": "r", "actor": "a"}});
        let error = method.check(&envelope).unwrap_err();
        assert_eq!(error.parameter.as_deref(), Some(MUTATION));
        assert!(error.message.contains("the adapter builds"), "{error}");
        assert!(!validator.is_valid(&envelope));
        let given = json!({"flag": true, "expected_revision": 3});
        match expected {
            ExpectedRevision::None => {
                let error = method.check(&given).unwrap_err();
                assert_eq!(error.parameter.as_deref(), Some(EXPECTED_REVISION));
                assert!(!validator.is_valid(&given));
            }
            _ => {
                let checked = method.check(&given).unwrap();
                assert_eq!(checked.expected_revision, Some(3));
                assert_eq!(checked.params, *json!({"flag": true}).as_object().unwrap());
                assert!(validator.is_valid(&given));
                let negative = json!({"expected_revision": -1});
                let error = method.check(&negative).unwrap_err();
                assert_eq!(
                    error.message,
                    "parameter expected_revision must be a non-negative integer"
                );
                assert!(!validator.is_valid(&negative));
            }
        }
        let omitted = method.check(&json!({"flag": true}));
        assert_eq!(
            omitted.is_ok(),
            expected != ExpectedRevision::Required,
            "{omitted:?}"
        );
        if let Err(error) = omitted {
            assert_eq!(
                error.message,
                "missing required parameter expected_revision for test.method"
            );
        }
    }
}

/// Arguments that are not an object are refused without naming a parameter; absent arguments
/// are an empty object.
#[test]
fn arguments_must_be_an_object() {
    let method = input(&host_entry(&[ParameterDescriptor::boolean("flag")]));
    assert_eq!(method.check(&Value::Null).unwrap().params, Map::new());
    assert_eq!(
        method.check(&json!([true])).unwrap_err(),
        ArgumentError {
            parameter: None,
            message: "the arguments of test.method must be a JSON object".into(),
        }
    );
}

/// The registries' generated inputs, by name.
fn registry_inputs(registry: &ModuleRegistry) -> (Value, MethodInputs) {
    let listed = schemas(registry);
    let inputs = MethodInputs::read(&listed).expect("every listed method reads");
    (listed, inputs)
}

/// A value of `parameter`'s kind that its check accepts: its default where it has one, which is
/// then checked like any value, and otherwise the least value its kind allows.
fn example(parameter: &ParameterDescriptor) -> Value {
    if let Some(default) = parameter.default.clone().filter(|value| !value.is_null()) {
        return default;
    }
    let hex = "0123456789abcdef0123456789abcdef";
    match &parameter.kind {
        ParameterKind::Integer { min, .. } => json!(min),
        ParameterKind::Number { min, .. } => json!(min),
        ParameterKind::Enum { options } => json!(options[0]),
        ParameterKind::Boolean => json!(true),
        ParameterKind::String { .. } => json!(""),
        ParameterKind::Text { .. } => json!("/tmp/example"),
        ParameterKind::Identity { of } => match of {
            IdentityKind::Asset => json!(AssetId::new()),
            IdentityKind::Entry => json!(EntryId::new()),
            IdentityKind::Draft => json!(DraftId::new()),
            IdentityKind::Job => json!(JobId::new()),
            IdentityKind::Preset => json!(PresetId::new()),
            IdentityKind::Mask => json!(MaskId::new()),
            IdentityKind::Component => json!(ComponentId::new()),
            IdentityKind::Stroke => json!(hex),
            IdentityKind::CatalogFolder => json!(format!("folder-{hex}")),
            IdentityKind::Collection => json!(format!("collection-{hex}")),
            IdentityKind::Correction => json!(CorrectionId::new(1.try_into().unwrap())),
        },
        ParameterKind::Artifact => json!(format!("artifact-{hex}{hex}")),
        ParameterKind::Points { points_min, .. } => json!(vec![[0.5, 0.5]; *points_min]),
        ParameterKind::Curve {
            points_min,
            fixed_x,
            ..
        } => {
            let xs = fixed_x.clone().unwrap_or_else(|| {
                (0..*points_min)
                    .map(|index| index as f64 / (*points_min - 1).max(1) as f64)
                    .collect()
            });
            json!(xs.iter().map(|x| [*x, *x]).collect::<Vec<_>>())
        }
        ParameterKind::Color => json!([1, 2, 3]),
        ParameterKind::Settings => json!({"set-basic": {"exposure": 0.5}}),
        ParameterKind::SettingsOrigin => json!({"kind": "preset", "name": "Warm"}),
        ParameterKind::Endpoint { .. } => json!("https://example.com/"),
        ParameterKind::Secret { .. } => json!("key"),
        ParameterKind::Json => json!({}),
    }
}

/// Every method of the built-in and developer registries generates a valid 2020-12 schema whose
/// properties are exactly its inputs. For each, a request naming only its required arguments and
/// one naming every argument are accepted by the adapter and the schema alike, and an unknown
/// property, or a required argument left out, is refused by both, naming it.
#[test]
fn every_registry_method_generates_a_schema_its_requests_meet() {
    for registry in [ModuleRegistry::builtin(), ModuleRegistry::developer()] {
        let (listed, inputs) = registry_inputs(&registry);
        let listed = listed["methods"].as_object().unwrap();
        assert_eq!(inputs.len(), listed.len());
        for method in inputs.iter() {
            let name = method.method();
            let entry = &listed[name];
            let schema = method.input_schema();
            let validator = validator(&schema);
            assert_eq!(schema["$schema"], DIALECT, "{name}");
            assert_eq!(schema["additionalProperties"], false, "{name}");
            let properties = schema["properties"].as_object().unwrap();
            assert!(!properties.contains_key(MUTATION), "{name}");
            // Every listed field but the envelope is a property, and nothing else is.
            let mut fields: Vec<&str> = entry["required"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(Value::as_str)
                .chain(
                    entry["optional"]
                        .as_object()
                        .unwrap()
                        .keys()
                        .map(String::as_str),
                )
                .filter(|field| *field != MUTATION)
                .collect();
            if method.expected_revision() != ExpectedRevision::None {
                fields.push(EXPECTED_REVISION);
            }
            fields.sort_unstable();
            let mut listed_properties: Vec<&str> = properties.keys().map(String::as_str).collect();
            listed_properties.sort_unstable();
            assert_eq!(listed_properties, fields, "{name}");
            assert_eq!(
                method.expected_revision(),
                match entry.get("revision_of").and_then(Value::as_str) {
                    None => ExpectedRevision::None,
                    Some("asset") => ExpectedRevision::Optional,
                    Some(_) => ExpectedRevision::Required,
                },
                "{name}"
            );
            let required: Vec<&str> = schema["required"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(Value::as_str)
                .collect();
            let mut minimal = Map::new();
            let mut full = Map::new();
            for property in &method.properties {
                let value = example(&property.parameter);
                if property.required {
                    minimal.insert(property.parameter.name.clone(), value.clone());
                }
                full.insert(property.parameter.name.clone(), value);
            }
            if method.expected_revision() != ExpectedRevision::None {
                full.insert(EXPECTED_REVISION.into(), json!(4));
                if method.expected_revision() == ExpectedRevision::Required {
                    minimal.insert(EXPECTED_REVISION.into(), json!(4));
                }
            }
            for arguments in [Value::Object(minimal.clone()), Value::Object(full)] {
                if let Err(error) = method.check(&arguments) {
                    panic!("{name} refuses {arguments}: {error}");
                }
                assert!(validator.is_valid(&arguments), "{name}: {arguments}");
            }
            let mut unknown = minimal.clone();
            unknown.insert("not_a_parameter".into(), json!(1));
            let unknown = Value::Object(unknown);
            let error = method.check(&unknown).unwrap_err();
            assert_eq!(
                error.parameter.as_deref(),
                Some("not_a_parameter"),
                "{name}"
            );
            assert!(!validator.is_valid(&unknown), "{name}");
            for field in required {
                let mut missing = minimal.clone();
                missing.remove(field);
                let missing = Value::Object(missing);
                let error = method.check(&missing).unwrap_err();
                assert_eq!(error.parameter.as_deref(), Some(field), "{name}");
                assert_eq!(
                    error.message,
                    format!("missing required parameter {field} for {name}")
                );
                assert!(!validator.is_valid(&missing), "{name}");
            }
        }
    }
}

/// Requests the registry's own tests send, as an agent would give them: the envelope left out and
/// a revision given as `expected_revision`. The adapter and the schema both accept each.
#[test]
fn requests_the_registry_tests_send_are_accepted() {
    let (_, inputs) = registry_inputs(&ModuleRegistry::developer());
    let asset = AssetId::new();
    let artifact = format!("artifact-{}", "0123456789abcdef".repeat(4));
    let requests = [
        (
            "edit.set-basic",
            json!({"asset_id": asset, "exposure": 0.5, "expected_revision": 0}),
        ),
        ("edit.set-basic", json!({"asset_id": asset, "exposure": 1})),
        ("history.list", json!({"asset_id": asset, "limit": 8})),
        (
            "history.undo",
            json!({"asset_id": asset, "expected_revision": 1}),
        ),
        ("render.sample", json!({"asset_id": asset, "x": 0, "y": 0})),
        ("job.wait", json!({"job_id": JobId::new()})),
        ("artifact.inspect", json!({"artifact_id": artifact})),
        ("query.auto-tone", json!({"asset_id": asset})),
        (
            "draft.begin",
            json!({"asset_id": asset, "action": "accept-correction-patch"}),
        ),
        (
            "draft.set",
            json!({"draft_id": DraftId::new(), "fields": {"job_id": JobId::new(), "candidate": 0}}),
        ),
        (
            "draft.commit",
            json!({"draft_id": DraftId::new(), "expected_revision": 0}),
        ),
        (
            "preferences.set",
            json!({"auto_collapse_history": false, "raw_look": "neutral", "canvas_background": "black"}),
        ),
        ("preferences.set", json!({"interface_size": 150})),
        ("preferences.set", json!({"theme": null})),
        (
            "preferences.set",
            json!({"update_checks": null, "update_install_automatically": null}),
        ),
        (
            "preset.create",
            json!({"name": "Warm", "settings": {"set-basic": {"exposure": 0.5}}}),
        ),
        (
            "edit.apply-settings",
            json!({"asset_id": asset, "settings": {"set-basic": {"exposure": 0.5}}, "origin": {"kind": "paste", "source": "DSC_0001.NEF"}, "expected_revision": 1}),
        ),
        (
            "batch.apply-settings",
            json!({"targets": {"kind": "assets", "asset_ids": [asset]}, "preset_id": PresetId::new()}),
        ),
        (
            "edit.add-correction",
            json!({"asset_id": asset, "points": [[0.1, 0.1], [0.12, 0.1]], "radius": 0.02, "source-x": 0.3, "source-y": 0.3, "expected_revision": 2}),
        ),
        (
            "mask.add-stroke",
            json!({"asset_id": asset, "points": [[0.2, 0.2], [0.3, 0.25]], "size": 0.05, "feather": 50, "flow": 100, "erase": false}),
        ),
        (
            "edit.crop",
            json!({"asset_id": asset, "x": 0.1, "y": 0.1, "width": 0.8, "height": 0.8, "angle": 2.5}),
        ),
        (
            "edit.set-curve",
            json!({"asset_id": asset, "luminance": [[0.0, 0.0], [0.5, 0.6], [1.0, 1.0]]}),
        ),
        (
            "edit.set-controls",
            json!({"asset_id": asset, "rgb": [10, 20, 30], "mode": "two"}),
        ),
        (
            "module.settings.set-secret",
            json!({"module_id": "example", "profile_id": "profile-1", "setting": "api-key", "value": "secret", "expected_revision": 0}),
        ),
        ("browse.view", json!({"source": {"kind": "catalog"}})),
        (
            "export.plan",
            json!({"asset_id": asset, "settings": {"format": "jpeg"}}),
        ),
        (
            "preview.compare",
            json!({"asset_id": asset, "enabled": true, "position": 0.5}),
        ),
    ];
    for (name, arguments) in requests {
        let method = inputs
            .get(name)
            .unwrap_or_else(|| panic!("{name} is listed"));
        if let Err(error) = method.check(&arguments) {
            panic!("{name} refuses {arguments}: {error}");
        }
        assert!(
            jsonschema::is_valid(&method.input_schema(), &arguments),
            "{name}'s schema refuses {arguments}"
        );
    }
}

/// Requests the registry refuses are refused before sending, naming the parameter: out of range,
/// of the wrong kind or unknown. The schema refuses each too, except a text bound in bytes, which
/// only the adapter can check.
#[test]
fn registry_requests_out_of_range_of_the_wrong_kind_or_unknown_are_refused() {
    let (_, inputs) = registry_inputs(&ModuleRegistry::builtin());
    let asset = AssetId::new();
    let refusals = [
        (
            "edit.set-basic",
            json!({"asset_id": asset, "exposure": 9}),
            "exposure",
            "parameter exposure must be a number within -5..=5",
            true,
        ),
        (
            "history.list",
            json!({"asset_id": asset, "limit": "8"}),
            "limit",
            "parameter limit must be an integer",
            true,
        ),
        (
            "history.list",
            json!({"asset_id": asset, "colour": 1}),
            "colour",
            "unknown parameter colour for history.list",
            true,
        ),
        (
            "render.sample",
            json!({"asset_id": asset, "x": 0}),
            "y",
            "missing required parameter y for render.sample",
            true,
        ),
        (
            "render.sample",
            json!({"asset_id": "entry-0123456789", "x": 0, "y": 0}),
            "asset_id",
            "parameter asset_id must be an asset identity",
            false,
        ),
        (
            "edit.set-basic",
            json!({"asset_id": asset, "exposure": null}),
            "exposure",
            "parameter exposure must be a number",
            true,
        ),
        (
            "draft.commit",
            json!({"draft_id": DraftId::new()}),
            "expected_revision",
            "missing required parameter expected_revision for draft.commit",
            true,
        ),
        (
            "preferences.set",
            json!({"catalog": "é".repeat(2049)}),
            "catalog",
            "parameter catalog must be at most 4096 bytes",
            false,
        ),
        (
            "preferences.set",
            json!({"update_skipped_version": null}),
            "update_skipped_version",
            "unknown parameter update_skipped_version for preferences.set",
            true,
        ),
        (
            "edit.add-correction",
            json!({"asset_id": asset, "points": [], "radius": 0.02}),
            "points",
            "parameter points must hold at least 1 positions",
            true,
        ),
    ];
    for (name, arguments, parameter, message, schema_refuses) in refusals {
        let method = inputs.get(name).unwrap();
        let error = method.check(&arguments).unwrap_err();
        assert_eq!(
            (error.parameter.as_deref(), error.message.as_str()),
            (Some(parameter), message),
            "{name}"
        );
        assert_eq!(
            !jsonschema::is_valid(&method.input_schema(), &arguments),
            schema_refuses,
            "{name}: whether the schema refuses {arguments}"
        );
    }
}
