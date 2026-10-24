//! Input schemas and the request check, generated from `schema.list`
//! ([schema generation](../../../../docs/design/mcp.md#schema-generation)).
//!
//! Each method's `schema.list` entry, in the JSON form a live session answers, becomes a
//! [`MethodInput`]: its declared parameters, typed with the one parameter vocabulary, and the host
//! fields its entry lists beside them (`asset_id`, `entry_id`, `profile_id` and a maskable
//! module's `target`). From that the adapter draws two things that cannot disagree:
//!
//! - [`MethodInput::input_schema`], a JSON Schema 2020-12 object schema with `required` and
//!   `additionalProperties: false`, for a tool's `inputSchema`; and
//! - [`MethodInput::check`], the check the adapter runs before it sends anything, which refuses an
//!   unknown, missing or ill-typed argument with an error naming it. It checks each value with the
//!   core's own [`check_value`], so it refuses in the owner's words, and covers what a schema
//!   cannot say, such as a `text` parameter's bound in bytes.
//!
//! The mutation envelope never appears: the adapter builds `mutation` itself. A method whose
//! envelope carries a revision gains one `expected_revision` property instead, optional where the
//! revision is an asset's (the adapter reads `asset.state` when it is left out) and required for a
//! draft's or a module's settings ([`ExpectedRevision`]).
//!
//! A parameter kind this build does not know is refused when the entry is read, never mapped to an
//! empty schema, and every kind the core declares is matched exhaustively, so a new kind cannot
//! build without its mapping.
use luxforge_core::{
    Envelope, IdentityKind, ParameterDescriptor, ParameterKind, RevisionOf, check_value,
};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::fmt;

/// The dialect every generated schema declares.
pub const DIALECT: &str = "https://json-schema.org/draft/2020-12/schema";

/// The property an agent gives a method's revision in. The adapter puts it in the envelope.
pub const EXPECTED_REVISION: &str = "expected_revision";

/// The envelope field, which the adapter builds and an agent never passes.
const MUTATION: &str = "mutation";

/// The longest plain name a host field such as `profile_id` carries, the host's `string` kind's
/// own limit (`luxforge_core`'s `kind::name()`).
const MAX_NAME: usize = 256;

/// A `schema.list` answer, or one entry of it, that this adapter cannot read: a kind, envelope or
/// field it does not know, or a malformed entry. The method is never offered rather than offered
/// with a guessed schema.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SchemaError(pub String);

impl fmt::Display for SchemaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SchemaError {}

/// Arguments the adapter refuses before sending anything, naming the parameter where there is one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArgumentError {
    /// The argument refused; `None` when the arguments are not an object at all.
    pub parameter: Option<String>,
    pub message: String,
}

impl fmt::Display for ArgumentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ArgumentError {}

/// Whether a method's tool has an `expected_revision` property, and whether it must be given.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExpectedRevision {
    /// The method carries no revision envelope.
    None,
    /// The revision is the asset's (`revision_of: asset`): used as given, or read from
    /// `asset.state` just before the call when left out.
    Optional,
    /// The revision is a draft's asset's or a module's settings' (`revision_of: draft` or
    /// `module-settings`), which the adapter cannot read for the agent.
    Required,
}

/// Arguments [`MethodInput::check`] accepted: the method's parameters, without the envelope, and
/// the revision the agent gave for it.
#[derive(Clone, Debug, PartialEq)]
pub struct Checked {
    pub params: Map<String, Value>,
    pub expected_revision: Option<u64>,
}

/// Every method of one `schema.list` answer, by name.
#[derive(Clone, Debug)]
pub struct MethodInputs {
    methods: BTreeMap<String, MethodInput>,
}

impl MethodInputs {
    /// Read every method of a `schema.list` answer. One entry the adapter cannot read refuses the
    /// whole answer, naming it, so no method is offered with a guessed schema.
    pub fn read(schema_list: &Value) -> Result<Self, SchemaError> {
        let range = path_range(schema_list)?;
        let methods = schema_list
            .get("methods")
            .and_then(Value::as_object)
            .ok_or_else(|| SchemaError("schema.list lists no methods object".into()))?;
        let methods = methods
            .iter()
            .map(|(name, entry)| Ok((name.clone(), MethodInput::read(name, entry, range)?)))
            .collect::<Result<_, SchemaError>>()?;
        Ok(Self { methods })
    }

    pub fn get(&self, method: &str) -> Option<&MethodInput> {
        self.methods.get(method)
    }

    /// Every method, by name.
    pub fn iter(&self) -> impl Iterator<Item = &MethodInput> {
        self.methods.values()
    }

    pub fn len(&self) -> usize {
        self.methods.len()
    }

    pub fn is_empty(&self) -> bool {
        self.methods.is_empty()
    }
}

/// The range a `points` position's coordinates lie in, which `schema.list` publishes once in
/// `paths.range`.
fn path_range(schema_list: &Value) -> Result<[f64; 2], SchemaError> {
    <[f64; 2]>::deserialize(&schema_list["paths"]["range"])
        .ok()
        .filter(|[min, max]| min.is_finite() && max.is_finite() && min <= max)
        .ok_or_else(|| SchemaError("schema.list publishes no paths.range [min, max]".into()))
}

/// One method's input: its properties in order, the envelope it carries and whose revision that
/// envelope checks.
#[derive(Clone, Debug)]
pub struct MethodInput {
    method: String,
    envelope: Envelope,
    expected_revision: ExpectedRevision,
    revision_of: Option<RevisionOf>,
    properties: Vec<Property>,
    path_range: [f64; 2],
}

/// One argument a method takes: a declared parameter or a host field its entry lists beside them.
#[derive(Clone, Debug)]
struct Property {
    parameter: ParameterDescriptor,
    required: bool,
    /// Whether `null` is a value: an optional field of a host method, whose parse reads `null` as
    /// absent or, for a field that can be cleared, as clearing it. A generated method's check
    /// (an action's, query's or task's) refuses `null` as any other wrong kind.
    nullable: bool,
}

impl MethodInput {
    /// Read `method`'s `schema.list` entry, with the path coordinate range its answer publishes.
    pub fn read(method: &str, entry: &Value, path_range: [f64; 2]) -> Result<Self, SchemaError> {
        let refuse = |what: String| SchemaError(format!("{method}'s schema entry {what}"));
        let named = entry.get(MUTATION).map(|name| name.as_str().unwrap_or("?"));
        let envelope = Envelope::named(named)
            .ok_or_else(|| refuse(format!("names the envelope {}", entry[MUTATION])))?;
        let revision_of = entry
            .get("revision_of")
            .map(|of| {
                RevisionOf::deserialize(of)
                    .map_err(|_| refuse(format!("names the revision of {of}")))
            })
            .transpose()?;
        let expected_revision = match (envelope, revision_of) {
            (Envelope::None | Envelope::Request, None) => ExpectedRevision::None,
            (Envelope::Revision, Some(RevisionOf::Asset)) => ExpectedRevision::Optional,
            (Envelope::Revision, Some(RevisionOf::Draft | RevisionOf::ModuleSettings)) => {
                ExpectedRevision::Required
            }
            _ => {
                return Err(refuse(
                    "pairs its envelope and revision_of differently".into(),
                ));
            }
        };
        let declared = entry
            .get("parameters")
            .and_then(Value::as_array)
            .ok_or_else(|| refuse("lists no parameters array".into()))?
            .iter()
            .map(|parameter| {
                let name = parameter["name"].as_str().unwrap_or("?");
                ParameterDescriptor::deserialize(parameter)
                    .map_err(|error| refuse(format!("declares parameter {name}: {error}")))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let required: Vec<&str> = entry
            .get("required")
            .and_then(Value::as_array)
            .and_then(|names| names.iter().map(Value::as_str).collect())
            .ok_or_else(|| refuse("lists no required array of names".into()))?;
        let optional = entry
            .get("optional")
            .and_then(Value::as_object)
            .ok_or_else(|| refuse("lists no optional object".into()))?;
        let generated = is_generated(method, entry, &declared);
        let target = entry
            .get("target")
            .map(|target| {
                ParameterDescriptor::deserialize(target)
                    .map_err(|error| refuse(format!("declares its target: {error}")))
            })
            .transpose()?;
        // A field listed beside the declared parameters: the envelope, which is the adapter's, or a
        // host field, typed here because the entry lists it by name only.
        let field = |name: &str, required: bool| -> Result<Option<Property>, SchemaError> {
            if name == MUTATION {
                return Ok(None);
            }
            let mut parameter = match declared.iter().find(|parameter| parameter.name == name) {
                Some(parameter) => parameter.clone(),
                None => host_field(name, target.as_ref()).ok_or_else(|| {
                    refuse(format!(
                        "lists {name}, which no parameter or host field types"
                    ))
                })?,
            };
            // An optional field's notes are the entry's own words for it here, such as what a
            // query's mask target asks about.
            if let Some(notes) = optional.get(name).and_then(Value::as_str)
                && !notes.is_empty()
            {
                parameter.notes = notes.to_owned();
            }
            Ok(Some(Property {
                parameter,
                required,
                nullable: !required && !generated,
            }))
        };
        let mut properties = Vec::with_capacity(declared.len() + 2);
        for name in &required {
            properties.extend(field(name, true)?);
        }
        for parameter in &declared {
            if !required.contains(&parameter.name.as_str()) {
                if !optional.contains_key(&parameter.name) {
                    return Err(refuse(format!(
                        "declares parameter {} but lists it neither required nor optional",
                        parameter.name
                    )));
                }
                properties.extend(field(&parameter.name, false)?);
            }
        }
        for name in optional.keys() {
            if !declared.iter().any(|parameter| &parameter.name == name) {
                properties.extend(field(name, false)?);
            }
        }
        if envelope != Envelope::None && !required.contains(&MUTATION) {
            return Err(refuse("names an envelope it does not require".into()));
        }
        for (index, property) in properties.iter().enumerate() {
            let name = property.parameter.name.as_str();
            if name == EXPECTED_REVISION
                || properties[..index]
                    .iter()
                    .any(|earlier| earlier.parameter.name == name)
            {
                return Err(refuse(format!("lists {name} twice")));
            }
        }
        Ok(Self {
            method: method.to_owned(),
            envelope,
            expected_revision,
            revision_of,
            properties,
            path_range,
        })
    }

    /// The method's name, as `schema.list` lists it.
    pub fn method(&self) -> &str {
        &self.method
    }

    /// The envelope the adapter builds for a call.
    pub fn envelope(&self) -> Envelope {
        self.envelope
    }

    /// Whether the tool takes `expected_revision`, and whether it must.
    pub fn expected_revision(&self) -> ExpectedRevision {
        self.expected_revision
    }

    /// Whose revision `expected_revision` is, as the entry publishes it.
    pub fn revision_of(&self) -> Option<RevisionOf> {
        self.revision_of
    }

    /// The method's input as a JSON Schema 2020-12 object schema: one property per argument, the
    /// required ones listed in `required`, and no other property allowed.
    pub fn input_schema(&self) -> Value {
        let mut properties = Map::new();
        let mut required = Vec::new();
        for property in &self.properties {
            properties.insert(
                property.parameter.name.clone(),
                property_schema(property, self.path_range),
            );
            if property.required {
                required.push(json!(property.parameter.name));
            }
        }
        if let Some((schema, needed)) = revision_schema(self.expected_revision, self.revision_of) {
            properties.insert(EXPECTED_REVISION.to_owned(), schema);
            if needed {
                required.push(json!(EXPECTED_REVISION));
            }
        }
        json!({
            "$schema": DIALECT,
            "type": "object",
            "properties": properties,
            "required": required,
            "additionalProperties": false,
        })
    }

    /// Check a call's arguments before anything is sent: an object naming only the method's
    /// properties, each value of its declared kind and within its bounds, and every required one
    /// present. Returns the parameters to send, without the revision, which the envelope carries.
    pub fn check(&self, arguments: &Value) -> Result<Checked, ArgumentError> {
        let empty = Map::new();
        let object = match arguments {
            Value::Object(object) => object,
            Value::Null => &empty,
            _ => {
                return Err(ArgumentError {
                    parameter: None,
                    message: format!("the arguments of {} must be a JSON object", self.method),
                });
            }
        };
        let refuse = |name: &str, message: String| ArgumentError {
            parameter: Some(name.to_owned()),
            message,
        };
        let mut params = Map::new();
        let mut expected_revision = None;
        for (name, value) in object {
            if name == EXPECTED_REVISION && self.expected_revision != ExpectedRevision::None {
                expected_revision = Some(value.as_u64().ok_or_else(|| {
                    refuse(
                        name,
                        format!("parameter {name} must be a non-negative integer"),
                    )
                })?);
                continue;
            }
            let Some(property) = self.property(name) else {
                return Err(refuse(name, self.unknown(name)));
            };
            check_property(property, value).map_err(|message| refuse(name, message))?;
            params.insert(name.clone(), value.clone());
        }
        let missing = self
            .properties
            .iter()
            .filter(|property| property.required)
            .map(|property| property.parameter.name.as_str())
            .chain(
                (self.expected_revision == ExpectedRevision::Required).then_some(EXPECTED_REVISION),
            )
            .find(|name| !object.contains_key(*name));
        if let Some(name) = missing {
            return Err(refuse(
                name,
                format!("missing required parameter {name} for {}", self.method),
            ));
        }
        Ok(Checked {
            params,
            expected_revision,
        })
    }

    fn property(&self, name: &str) -> Option<&Property> {
        self.properties
            .iter()
            .find(|property| property.parameter.name == name)
    }

    fn unknown(&self, name: &str) -> String {
        match (name, self.envelope) {
            (MUTATION, Envelope::Revision | Envelope::Request) => format!(
                "parameter {name} is not an argument: the adapter builds {}'s envelope{}",
                self.method,
                match self.expected_revision {
                    ExpectedRevision::None => "",
                    _ => "; give its revision as expected_revision",
                }
            ),
            (EXPECTED_REVISION, _) => {
                format!("unknown parameter {name}: {} has no revision", self.method)
            }
            _ => format!("unknown parameter {name} for {}", self.method),
        }
    }
}

/// Whether the entry is a generated method's — a module's or the host's action, query or task —
/// whose parameters the generic check reads, which refuses `null` as a value. An action lists
/// whether it is a `patch`, a task's name is `task.<id>`, and a query lists the `entry_id` it
/// reads beside its declared parameters. Every other method is the host's own, whose parse reads
/// an optional `null` as absent.
fn is_generated(method: &str, entry: &Value, declared: &[ParameterDescriptor]) -> bool {
    entry.get("patch").is_some()
        || method.starts_with("task.")
        || (entry["optional"].get("entry_id").is_some()
            && !declared
                .iter()
                .any(|parameter| parameter.name == "entry_id"))
}

/// The host fields an entry lists by name beside its declared parameters, typed as the host
/// declares them where it takes them: the asset an action, query or task acts on, the entry a
/// query reads, the provider profile a task names, and a maskable module's mask target, which the
/// entry declares itself in `target`.
fn host_field(name: &str, target: Option<&ParameterDescriptor>) -> Option<ParameterDescriptor> {
    let identity = |of| ParameterKind::Identity { of };
    let kind = match name {
        "asset_id" => identity(IdentityKind::Asset),
        "entry_id" => identity(IdentityKind::Entry),
        "profile_id" => ParameterKind::String {
            max_length: MAX_NAME,
        },
        _ => return target.filter(|target| target.name == name).cloned(),
    };
    Some(ParameterDescriptor::new(name, kind))
}

/// The `expected_revision` property, and whether it is required.
fn revision_schema(
    expected: ExpectedRevision,
    revision_of: Option<RevisionOf>,
) -> Option<(Value, bool)> {
    let whose = match revision_of? {
        RevisionOf::Asset => {
            "The revision of the asset asset_id names, as asset.state answers it. Left out, the \
             adapter reads asset.state just before the call"
        }
        RevisionOf::Draft => {
            "The revision of the asset the draft draft_id names is bound to, as asset.state \
             answers it"
        }
        RevisionOf::ModuleSettings => {
            "The revision of the settings of the module module_id names, as module.settings.read \
             answers it"
        }
    };
    let required = match expected {
        ExpectedRevision::None => return None,
        ExpectedRevision::Optional => false,
        ExpectedRevision::Required => true,
    };
    let schema = json!({
        "type": "integer",
        "minimum": 0,
        "description": format!(
            "{whose}. A different current revision is refused as a conflict, and the call is \
             never retried."
        ),
    });
    Some((schema, required))
}

/// One property's schema: its kind's, with its description and default, and `null` allowed where
/// the method reads it as absent.
fn property_schema(property: &Property, path_range: [f64; 2]) -> Value {
    let parameter = &property.parameter;
    let (mut schema, phrase) = kind_schema(&parameter.kind, path_range);
    if property.nullable {
        allow_null(&mut schema);
    }
    let description = describe(parameter, phrase);
    if !description.is_empty() {
        schema.insert("description".into(), description.into());
    }
    if let Some(default) = parameter.default.as_ref().filter(|value| !value.is_null()) {
        schema.insert("default".into(), default.clone());
    }
    Value::Object(schema)
}

/// A kind's schema and what its description says of the kind itself, where the schema cannot say
/// it. Exhaustive, so a kind the core adds does not build without its mapping.
fn kind_schema(
    kind: &ParameterKind,
    [path_min, path_max]: [f64; 2],
) -> (Map<String, Value>, String) {
    let pair = |min: f64, max: f64| {
        json!({
            "type": "array",
            "prefixItems": [
                {"type": "number", "minimum": min, "maximum": max},
                {"type": "number", "minimum": min, "maximum": max},
            ],
            "minItems": 2,
            "maxItems": 2,
        })
    };
    let (schema, phrase) = match kind {
        ParameterKind::Integer { min, max } => (
            json!({"type": "integer", "minimum": min, "maximum": max}),
            String::new(),
        ),
        ParameterKind::Number { min, max } => (
            json!({"type": "number", "minimum": min, "maximum": max}),
            String::new(),
        ),
        ParameterKind::Enum { options } => {
            (json!({"type": "string", "enum": options}), String::new())
        }
        ParameterKind::Boolean => (json!({"type": "boolean"}), String::new()),
        ParameterKind::String { max_length } => (
            json!({"type": "string", "maxLength": max_length}),
            "No control characters".to_owned(),
        ),
        ParameterKind::Text { max_bytes } => (
            json!({"type": "string"}),
            format!("UTF-8 text of at most {max_bytes} bytes, which may hold line breaks"),
        ),
        ParameterKind::Identity { of } => (json!({"type": "string"}), format!("{} id", noun(*of))),
        ParameterKind::Artifact => (
            json!({"type": "string"}),
            "An artifact id of a derived artifact published in this catalog".to_owned(),
        ),
        ParameterKind::Points {
            points_min,
            points_max,
        } => (
            json!({
                "type": "array",
                "items": pair(path_min, path_max),
                "minItems": points_min,
                "maxItems": points_max,
            }),
            "A path of [x, y] positions in the content stage's normalized coordinates, in drawn \
             order"
                .to_owned(),
        ),
        ParameterKind::Curve {
            points_min,
            points_max,
            monotone,
            fixed_x,
        } => {
            let mut phrase = "Curve points [x, y] within 0..1, x strictly increasing".to_owned();
            if *monotone {
                phrase.push_str(" and y never decreasing");
            }
            if let Some(xs) = fixed_x {
                let xs: Vec<String> = xs.iter().map(f64::to_string).collect();
                phrase.push_str(&format!(", at exactly the x positions {}", xs.join(", ")));
            }
            (
                json!({
                    "type": "array",
                    "items": pair(0.0, 1.0),
                    "minItems": points_min,
                    "maxItems": points_max,
                }),
                phrase,
            )
        }
        ParameterKind::Color => (
            json!({
                "type": "array",
                "items": {"type": "integer", "minimum": 0, "maximum": 255},
                "minItems": 3,
                "maxItems": 3,
            }),
            "Three 8-bit sRGB channels [r, g, b]".to_owned(),
        ),
        ParameterKind::Settings => (
            json!({
                "type": "object",
                "minProperties": 1,
                "additionalProperties": {"type": "object"},
            }),
            "A settings set: field-patch action identities, each with an object of that action's \
             fields"
                .to_owned(),
        ),
        ParameterKind::SettingsOrigin => {
            let identity = json!({"type": ["string", "null"]});
            let text = json!({"type": "string", "minLength": 1});
            (
                json!({
                    "oneOf": [
                        {
                            "type": "object",
                            "properties": {
                                "kind": {"const": "preset"},
                                "name": text,
                                "preset_id": identity,
                            },
                            "required": ["kind", "name"],
                            "additionalProperties": false,
                        },
                        {
                            "type": "object",
                            "properties": {
                                "kind": {"const": "paste"},
                                "source": text,
                                "source_asset": identity,
                            },
                            "required": ["kind", "source"],
                            "additionalProperties": false,
                        },
                    ],
                }),
                String::new(),
            )
        }
        ParameterKind::Endpoint { .. } => (
            json!({"type": "string"}),
            "A URL the host's transport policy accepts".to_owned(),
        ),
        ParameterKind::Secret { max_length } => (
            json!({"type": "string", "minLength": 1, "maxLength": max_length}),
            "A secret, kept only in the operating system's secret store".to_owned(),
        ),
        ParameterKind::Json => (json!({}), String::new()),
    };
    let Value::Object(schema) = schema else {
        unreachable!("every kind's schema is an object")
    };
    (schema, phrase)
}

/// The object an identity names, with its article.
fn noun(of: IdentityKind) -> &'static str {
    match of {
        IdentityKind::Asset => "An asset",
        IdentityKind::Entry => "An entry",
        IdentityKind::Draft => "A draft",
        IdentityKind::Job => "A job",
        IdentityKind::Preset => "A preset",
        IdentityKind::Mask => "A mask",
        IdentityKind::Component => "A mask component",
        IdentityKind::Stroke => "A stroke (content address)",
        IdentityKind::CatalogFolder => "A catalog folder",
        IdentityKind::Collection => "A collection",
        IdentityKind::Correction => "A correction",
    }
}

/// Let a schema accept `null` beside what it accepts: a `null` type, enum member or alternative.
fn allow_null(schema: &mut Map<String, Value>) {
    if let Some(Value::String(kind)) = schema.get("type") {
        let kind = kind.clone();
        schema.insert("type".into(), json!([kind, "null"]));
    }
    if let Some(Value::Array(options)) = schema.get_mut("enum") {
        options.push(Value::Null);
    }
    if let Some(Value::Array(alternatives)) = schema.get_mut("oneOf") {
        alternatives.push(json!({"type": "null"}));
    }
}

/// A property's description: what its kind says, then its notes, then the client hints, unit,
/// step, soft range and precision, each a sentence.
fn describe(parameter: &ParameterDescriptor, phrase: String) -> String {
    let number = |value: f64| value.to_string();
    let soft = match (parameter.soft_min, parameter.soft_max) {
        (Some(min), Some(max)) => Some(format!("Soft range {} to {}", number(min), number(max))),
        (Some(min), None) => Some(format!("Soft minimum {}", number(min))),
        (None, Some(max)) => Some(format!("Soft maximum {}", number(max))),
        (None, None) => None,
    };
    [
        Some(phrase),
        Some(parameter.notes.clone()),
        parameter.unit.as_ref().map(|unit| format!("Unit: {unit}")),
        parameter.step.map(|step| format!("Step {}", number(step))),
        soft,
        parameter
            .precision
            .map(|precision| format!("Shown to {precision} decimal places")),
    ]
    .into_iter()
    .flatten()
    .filter_map(|part| {
        let part = part.trim().trim_end_matches('.');
        let mut chars = part.chars();
        let first = chars.next()?;
        Some(format!("{}{}.", first.to_uppercase(), chars.as_str()))
    })
    .collect::<Vec<_>>()
    .join(" ")
}

/// One value against its property: `null` where the method reads it as absent, a secret as the
/// string its setting stores, and every other value by the core's own check, in its words.
fn check_property(property: &Property, value: &Value) -> Result<(), String> {
    let parameter = &property.parameter;
    let name = &parameter.name;
    match (&parameter.kind, value) {
        (_, Value::Null) if property.nullable => Ok(()),
        (ParameterKind::Json, _) => Ok(()),
        // The core never checks a secret as a plain value: its own type takes it without echoing
        // it, and its setting's limit is checked where it is stored.
        (ParameterKind::Secret { max_length }, value) => value
            .as_str()
            .filter(|secret| (1..=*max_length).contains(&secret.chars().count()))
            .map(|_| ())
            .ok_or_else(|| {
                format!("parameter {name} must be a string of 1..={max_length} characters")
            }),
        _ => check_value(parameter, value).map_err(|error| error.detail),
    }
}

#[cfg(test)]
mod tests;
