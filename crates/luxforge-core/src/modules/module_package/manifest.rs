//! `luxforge-module.toml`: an external module's identity, its one effect and its field table, read
//! as TOML, checked, and turned into a field-patch [`Spec`] whose descriptor the package carries.
//!
//! The TOML is converted to JSON values and read into private declaration types that refuse an
//! unknown key, so a misspelt hint is refused rather than ignored. The keys a field takes are the
//! existing parameter vocabulary's (`min`, `max`, `options`, `default`, `unit`, `step`,
//! `precision`, `soft_min`, `soft_max`, `fine_step`, `zero`, `notes`) and its control's (`label`,
//! `history`, `style`, `rail`); a group lists its fields by name and may declare wheels over them.
use super::external::{AdjustmentUnits, ExternalPatch};
use super::{MODULE_API, PackageRefusal};
use crate::Error;
use crate::modules::field_patch::{Field, FieldControl, FieldPatchModule, Group, Spec, Wheel};
use crate::modules::{
    ChoiceStyle, ColorStyle, EffectStage, MAX_SETTINGS_FIELDS, ModuleDescriptor, NumberStyle,
    ParameterDescriptor, RailDecoration, ToolModule, valid_identity, valid_name,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Number, Value};
use std::sync::Arc;

/// The one toolset open to external modules in this interface version: the tools panel's sections.
const ADJUSTMENTS: &str = "adjustments";
/// The toolsets a built-in module may call home, which this interface version does not open.
const CLOSED_TOOLSETS: [&str; 4] = ["corrections", "masking", "tools", "processing"];
/// The effect's identity is the module's with this last segment: an external effect is always in
/// its own module's namespace.
const EFFECT_SEGMENT: &str = "adjust";

const MAX_ID_CHARS: usize = 128;
const MAX_NAME_CHARS: usize = 48;
const MAX_TITLE_CHARS: usize = 48;
const MAX_HINT_CHARS: usize = 160;
const MAX_AUTHORS: usize = 16;
const MAX_AUTHOR_CHARS: usize = 128;
const MAX_LICENSE_CHARS: usize = 128;
const MAX_URL_CHARS: usize = 512;
const MAX_LABEL_CHARS: usize = 64;
const MAX_NOTES_CHARS: usize = 1024;
const MAX_UNIT_CHARS: usize = 16;
const MAX_GROUPS: usize = 16;
const MAX_WHEELS: usize = 4;
const MAX_OPTIONS: usize = 32;
/// The parameter kinds a field of an external table takes: the field-patch vocabulary less the
/// curve, whose control a manifest cannot declare yet.
const KINDS: [&str; 5] = ["number", "integer", "boolean", "enum", "color"];

/// An external module's manifest, checked: its identity and metadata, its one effect, and the
/// descriptor its field table builds.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ModuleManifest {
    /// The component interface version, [`MODULE_API`].
    pub api: String,
    /// The module's reverse-DNS identity, outside `luxforge.*` and `app.luxforge.*`.
    pub id: String,
    /// The noun of its actions, `set-<name>` and `reset-<name>`: the manifest's `name`, or the id's
    /// last segment.
    pub name: String,
    pub title: String,
    pub hint: String,
    /// A SemVer version.
    pub version: String,
    pub authors: Vec<String>,
    /// An SPDX licence expression, checked for its shape, not against the licence list.
    pub license: String,
    /// An `https` URL without credentials.
    pub repository: String,
    /// Where the module's interface lives: `adjustments`, the only toolset open to external
    /// modules.
    pub toolset: String,
    pub effect: ManifestEffect,
    #[serde(skip)]
    table: Table,
    #[serde(skip)]
    descriptor: ModuleDescriptor,
}

/// The module's one effect: its identity (`<id>.adjust`), stage, order and payload format. It is
/// never maskable, `single`, and exists on every source kind.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ManifestEffect {
    pub id: String,
    pub stage: EffectStage,
    pub order: u16,
    pub format: u32,
}

/// The manifest as declared, before its checks.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Declared {
    api: String,
    id: String,
    #[serde(default)]
    name: Option<String>,
    title: String,
    hint: String,
    version: String,
    authors: Vec<String>,
    license: String,
    repository: String,
    toolset: String,
    effect: DeclaredEffect,
    fields: Vec<FieldDeclaration>,
    groups: Vec<GroupDeclaration>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeclaredEffect {
    stage: String,
    #[serde(default)]
    order: u16,
    format: u32,
    #[serde(default)]
    maskable: Option<Value>,
    #[serde(default)]
    sources: Option<Value>,
}

/// One field as declared: its parameter and its control.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
struct FieldDeclaration {
    name: String,
    kind: String,
    label: String,
    #[serde(default)]
    history: Option<String>,
    #[serde(default)]
    notes: Option<String>,
    #[serde(default)]
    default: Option<Value>,
    #[serde(default)]
    min: Option<Value>,
    #[serde(default)]
    max: Option<Value>,
    #[serde(default)]
    options: Option<Vec<String>>,
    #[serde(default)]
    unit: Option<String>,
    #[serde(default)]
    step: Option<f64>,
    #[serde(default)]
    precision: Option<u8>,
    #[serde(default)]
    soft_min: Option<f64>,
    #[serde(default)]
    soft_max: Option<f64>,
    #[serde(default)]
    fine_step: Option<f64>,
    #[serde(default)]
    zero: Option<f64>,
    #[serde(default)]
    style: Option<String>,
    #[serde(default)]
    rail: Option<RailDecoration>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
struct GroupDeclaration {
    label: String,
    fields: Vec<String>,
    #[serde(default)]
    wheels: Vec<WheelDeclaration>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
struct WheelDeclaration {
    label: String,
    hue: String,
    saturation: String,
    #[serde(default)]
    luminance: Option<String>,
}

/// The field table as declared, kept to build the module's spec again when an engine supplies its
/// units.
#[derive(Clone, Debug, Default, PartialEq)]
struct Table {
    fields: Vec<FieldDeclaration>,
    groups: Vec<GroupDeclaration>,
}

fn manifest_error(detail: impl std::fmt::Display) -> Error {
    PackageRefusal::Manifest.error(format!("manifest: {detail}"))
}

fn table_error(detail: impl std::fmt::Display) -> Error {
    PackageRefusal::Table.error(format!("manifest: {detail}"))
}

/// A TOML value as JSON: a datetime has no JSON form and a non-finite float no field accepts.
fn json(value: &toml_edit::Value, key: &str) -> Result<Value, Error> {
    Ok(match value {
        toml_edit::Value::String(text) => Value::String(text.value().clone()),
        toml_edit::Value::Integer(integer) => Value::from(*integer.value()),
        toml_edit::Value::Float(float) => Number::from_f64(*float.value())
            .map(Value::Number)
            .ok_or_else(|| manifest_error(format!("{key} is not a finite number")))?,
        toml_edit::Value::Boolean(boolean) => Value::Bool(*boolean.value()),
        toml_edit::Value::Datetime(_) => {
            return Err(manifest_error(format!(
                "{key} is a date, which no key takes"
            )));
        }
        toml_edit::Value::Array(array) => Value::Array(
            array
                .iter()
                .map(|value| json(value, key))
                .collect::<Result<_, _>>()?,
        ),
        toml_edit::Value::InlineTable(table) => Value::Object(
            table
                .iter()
                .map(|(name, value)| Ok((name.to_owned(), json(value, &format!("{key}.{name}"))?)))
                .collect::<Result<_, Error>>()?,
        ),
    })
}

fn json_table(table: &toml_edit::Table, key: &str) -> Result<Map<String, Value>, Error> {
    table
        .iter()
        .map(|(name, item)| {
            let path = if key.is_empty() {
                name.to_owned()
            } else {
                format!("{key}.{name}")
            };
            let value = match item {
                toml_edit::Item::None => Value::Null,
                toml_edit::Item::Value(value) => json(value, &path)?,
                toml_edit::Item::Table(table) => Value::Object(json_table(table, &path)?),
                toml_edit::Item::ArrayOfTables(tables) => Value::Array(
                    tables
                        .iter()
                        .map(|table| json_table(table, &path).map(Value::Object))
                        .collect::<Result<_, _>>()?,
                ),
            };
            Ok((name.to_owned(), value))
        })
        .collect()
}

/// Text of 1 to `max` characters that is not blank and holds no control character, or what is
/// wrong with it.
fn text_fault(key: &str, text: &str, max: usize) -> Option<String> {
    let count = text.chars().count();
    if text.trim().is_empty() {
        Some(format!("{key} is empty"))
    } else if count > max {
        Some(format!("{key} is {count} characters, more than {max}"))
    } else if text.chars().any(char::is_control) {
        Some(format!("{key} holds a control character"))
    } else {
        None
    }
}

/// A metadata text the manifest refuses when [`text_fault`] finds one.
fn check_text(key: &str, text: &str, max: usize) -> Result<(), Error> {
    text_fault(key, text, max).map_or(Ok(()), |fault| Err(manifest_error(fault)))
}

/// A field-table text the table refuses when [`text_fault`] finds one.
fn check_label(key: &str, text: &str, max: usize) -> Result<(), Error> {
    text_fault(key, text, max).map_or(Ok(()), |fault| Err(table_error(fault)))
}

/// The shape of an SPDX licence expression: identifiers (`MIT`, `GPL-3.0-or-later`,
/// `LicenseRef-x`), the operators `AND`, `OR` and `WITH`, and balanced parentheses. The licence list
/// itself is not checked.
fn spdx_shaped(expression: &str) -> bool {
    let spaced = expression.replace('(', " ( ").replace(')', " ) ");
    let mut depth = 0usize;
    let mut expect_operand = true;
    for token in spaced.split_whitespace() {
        match token {
            "(" if expect_operand => depth += 1,
            ")" if !expect_operand && depth > 0 => depth -= 1,
            "AND" | "OR" | "WITH" if !expect_operand => expect_operand = true,
            _ if expect_operand
                && token
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+')) =>
            {
                expect_operand = false;
            }
            _ => return false,
        }
    }
    depth == 0 && !expect_operand
}

/// The id is reverse-DNS: at least a domain's two labels and the module's name, each a valid
/// identity segment, outside Luxforge's own namespaces.
fn check_id(id: &str) -> Result<(), Error> {
    if id.len() > MAX_ID_CHARS || !valid_identity(id) || id.split('.').count() < 3 {
        return Err(PackageRefusal::Identity.error(format!(
            "manifest: id {id:?} is not a reverse-DNS module identity: lowercase words of letters \
             and digits separated by dots, a domain's labels reversed then the module's name, such \
             as org.example.splittoning"
        )));
    }
    if id.split('.').next() == Some("luxforge") || id.starts_with("app.luxforge.") {
        return Err(PackageRefusal::ReservedIdentity.error(format!(
            "manifest: id {id} is in Luxforge's own namespace, which only built-in modules use"
        )));
    }
    Ok(())
}

impl ModuleManifest {
    /// The manifest this text declares, or the refusal of the first thing wrong with it. The api
    /// version is read first, so a manifest for another interface version is refused as that
    /// rather than for keys this version does not know.
    pub fn parse(text: &str) -> Result<Self, Error> {
        let document = toml_edit::Document::parse(text)
            .map_err(|error| manifest_error(format!("not TOML: {}", error.message())))?;
        let object = json_table(document.as_table(), "")?;
        match object.get("api") {
            Some(Value::String(api)) if api == MODULE_API => {}
            Some(Value::String(api)) => {
                return Err(PackageRefusal::Api.error(format!(
                    "manifest: api {api} is not offered by this build, which offers {MODULE_API}"
                )));
            }
            Some(_) => return Err(manifest_error("api is not a string")),
            None => return Err(manifest_error("api is missing")),
        }
        let declared: Declared =
            serde_json::from_value(Value::Object(object)).map_err(manifest_error)?;
        Self::check(declared)
    }

    fn check(declared: Declared) -> Result<Self, Error> {
        let Declared {
            api,
            id,
            name,
            title,
            hint,
            version,
            authors,
            license,
            repository,
            toolset,
            effect,
            fields,
            groups,
        } = declared;
        check_id(&id)?;
        let name = match name {
            Some(name) => {
                if name.chars().count() > MAX_NAME_CHARS || !valid_name(&name) {
                    return Err(PackageRefusal::Identity.error(format!(
                        "manifest: name {name:?} is not an action noun: lowercase words of letters \
                         and digits separated by hyphens, such as split-toning"
                    )));
                }
                name
            }
            None => id.rsplit('.').next().unwrap_or(&id).to_owned(),
        };
        if toolset != ADJUSTMENTS {
            let reason = if CLOSED_TOOLSETS.contains(&toolset.as_str()) {
                format!("toolset {toolset} is not open to external modules, only {ADJUSTMENTS}")
            } else {
                format!("unknown toolset {toolset:?}; an external module's is {ADJUSTMENTS}")
            };
            return Err(PackageRefusal::Toolset.error(format!("manifest: {reason}")));
        }
        let effect = check_effect(&id, effect)?;
        check_text("title", &title, MAX_TITLE_CHARS)?;
        check_text("hint", &hint, MAX_HINT_CHARS)?;
        semver::Version::parse(&version).map_err(|error| {
            manifest_error(format!(
                "version {version:?} is not a SemVer version: {error}"
            ))
        })?;
        if authors.is_empty() || authors.len() > MAX_AUTHORS {
            return Err(manifest_error(format!(
                "authors names {} people; it names 1 to {MAX_AUTHORS}",
                authors.len()
            )));
        }
        for author in &authors {
            check_text("an author", author, MAX_AUTHOR_CHARS)?;
        }
        check_text("license", &license, MAX_LICENSE_CHARS)?;
        if !spdx_shaped(&license) {
            return Err(manifest_error(format!(
                "license {license:?} is not an SPDX licence expression"
            )));
        }
        check_repository(&repository)?;
        let table = Table { fields, groups };
        let mut manifest = Self {
            api,
            id,
            name,
            title,
            hint,
            version,
            authors,
            license,
            repository,
            toolset,
            effect,
            table,
            descriptor: ModuleDescriptor::default(),
        };
        manifest.descriptor = manifest
            .spec()?
            .check()
            .map_err(|error| table_error(error.detail))?;
        manifest
            .descriptor
            .validate()
            .map_err(|error| table_error(error.detail))?;
        Ok(manifest)
    }

    /// The descriptor the field table builds: what the module registers, lists and draws.
    pub fn descriptor(&self) -> &ModuleDescriptor {
        &self.descriptor
    }

    /// This manifest's module, with its colour arithmetic supplied by `units`: a field-patch module
    /// of the manifest's table, whose descriptor is [`Self::descriptor`].
    pub fn module(&self, units: Arc<dyn AdjustmentUnits>) -> Result<Arc<dyn ToolModule>, Error> {
        let spec = self.spec()?;
        let module = FieldPatchModule::from_spec(ExternalPatch::new(units), spec)
            .map_err(|error| table_error(error.detail))?;
        Ok(Arc::new(module))
    }

    /// The field-patch spec of this manifest: its identity, its one effect and its table. The
    /// manifest's toolset is the descriptor's home toolset once the descriptor declares one.
    fn spec(&self) -> Result<Spec, Error> {
        let Table { fields, groups } = &self.table;
        if fields.len() > MAX_SETTINGS_FIELDS {
            return Err(table_error(format!(
                "{} fields; a module declares at most {MAX_SETTINGS_FIELDS}, the most a settings \
                 set captures",
                fields.len()
            )));
        }
        if groups.is_empty() || groups.len() > MAX_GROUPS {
            return Err(table_error(format!(
                "{} groups; a module declares 1 to {MAX_GROUPS}",
                groups.len()
            )));
        }
        let fields = fields.iter().map(field).collect::<Result<Vec<_>, _>>()?;
        let groups = groups.iter().map(group).collect::<Result<Vec<_>, _>>()?;
        let spec = Spec::named(
            self.id.clone(),
            self.name.clone(),
            self.title.clone(),
            self.hint.clone(),
            &self.effect.id,
            self.effect.stage,
        )
        .format(self.effect.format)
        .order(self.effect.order)
        .fields(fields);
        Ok(groups.into_iter().fold(spec, Spec::group))
    }
}

fn check_effect(id: &str, effect: DeclaredEffect) -> Result<ManifestEffect, Error> {
    let DeclaredEffect {
        stage,
        order,
        format,
        maskable,
        sources,
    } = effect;
    let stage = match stage.as_str() {
        "color" => EffectStage::Color,
        "finish" => EffectStage::Finish,
        "source" | "geometry" | "repair" | "restoration" | "spatial" => {
            return Err(PackageRefusal::Stage.error(format!(
                "manifest: an external effect is on the color or finish stage, not {stage}"
            )));
        }
        _ => {
            return Err(PackageRefusal::Stage.error(format!(
                "manifest: unknown stage {stage:?}; an external effect is on the color or finish \
                 stage"
            )));
        }
    };
    match maskable {
        None | Some(Value::Bool(false)) => {}
        Some(Value::Bool(true)) => {
            return Err(PackageRefusal::Maskable
                .error("manifest: an external effect is not maskable".to_owned()));
        }
        Some(_) => return Err(manifest_error("effect.maskable is not a boolean")),
    }
    if sources.is_some() {
        return Err(PackageRefusal::Sources.error(
            "manifest: an external effect exists on every source kind and declares no sources"
                .to_owned(),
        ));
    }
    if format == 0 {
        return Err(manifest_error(
            "effect.format is 0; a payload format starts at 1",
        ));
    }
    Ok(ManifestEffect {
        id: format!("{id}.{EFFECT_SEGMENT}"),
        stage,
        order,
        format,
    })
}

fn check_repository(repository: &str) -> Result<(), Error> {
    let refused = |reason: &str| {
        manifest_error(format!(
            "repository {repository:?} is not an https URL without credentials: {reason}"
        ))
    };
    if repository.len() > MAX_URL_CHARS {
        return Err(refused("too long"));
    }
    let url = url::Url::parse(repository).map_err(|error| refused(&error.to_string()))?;
    if url.scheme() != "https" || url.host_str().is_none() {
        return Err(refused("not https"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(refused("it holds credentials"));
    }
    Ok(())
}

/// A number from a field's `min` or `max`, refusing what is not one.
fn number(name: &str, key: &str, value: Option<&Value>) -> Result<f64, Error> {
    value.and_then(Value::as_f64).ok_or_else(|| {
        table_error(format!(
            "field {name}: a number field declares a numeric {key}"
        ))
    })
}

fn integer(name: &str, key: &str, value: Option<&Value>) -> Result<i64, Error> {
    value.and_then(Value::as_i64).ok_or_else(|| {
        table_error(format!(
            "field {name}: an integer field declares an integer {key}"
        ))
    })
}

/// A style of the control vocabulary, by its serialized name.
fn style<T: serde::de::DeserializeOwned>(
    name: &str,
    control: &str,
    style: &str,
) -> Result<T, Error> {
    serde_json::from_value(Value::String(style.to_owned())).map_err(|_| {
        table_error(format!(
            "field {name}: a {control} control has no style {style:?}"
        ))
    })
}

/// A number hint's setter on a parameter: its step, soft range, fine step or zero.
type NumberHint = fn(ParameterDescriptor, f64) -> ParameterDescriptor;

/// One declared field as the table's [`Field`]: its parameter of the declared kind with its hints,
/// and its control. A key its kind does not take is refused; what a field patch checks of the
/// result — a valid default, a control that draws the kind, a rail only on a number — the spec's
/// build refuses by name.
fn field(declared: &FieldDeclaration) -> Result<Field, Error> {
    let name = declared.name.as_str();
    check_label(
        &format!("field {name}'s label"),
        &declared.label,
        MAX_LABEL_CHARS,
    )?;
    let not_taken = |key: &str, kind: &str| {
        table_error(format!("field {name}: a {kind} field declares no {key}"))
    };
    let kind = declared.kind.as_str();
    let unknown_kind = || {
        table_error(format!(
            "field {name}: kind {kind:?} is not one of number, integer, boolean, enum and color"
        ))
    };
    if !KINDS.contains(&kind) {
        return Err(unknown_kind());
    }
    let numeric = matches!(kind, "number" | "integer");
    let declares = [
        ("min", declared.min.is_some()),
        ("max", declared.max.is_some()),
        ("unit", declared.unit.is_some()),
        ("step", declared.step.is_some()),
        ("precision", declared.precision.is_some()),
        ("soft_min", declared.soft_min.is_some()),
        ("soft_max", declared.soft_max.is_some()),
        ("fine_step", declared.fine_step.is_some()),
        ("zero", declared.zero.is_some()),
    ];
    if !numeric && let Some((key, _)) = declares.iter().find(|(_, declared)| *declared) {
        return Err(not_taken(key, kind));
    }
    if kind != "enum" && declared.options.is_some() {
        return Err(not_taken("options", kind));
    }
    let (parameter, control) = match kind {
        "number" => (
            ParameterDescriptor::number(
                name,
                number(name, "min", declared.min.as_ref())?,
                number(name, "max", declared.max.as_ref())?,
            ),
            declared
                .style
                .as_deref()
                .map(|chosen| {
                    style::<NumberStyle>(name, "number", chosen).map(FieldControl::Number)
                })
                .transpose()?,
        ),
        "integer" => (
            ParameterDescriptor::integer(
                name,
                integer(name, "min", declared.min.as_ref())?,
                integer(name, "max", declared.max.as_ref())?,
            ),
            declared
                .style
                .as_deref()
                .map(|chosen| {
                    style::<NumberStyle>(name, "number", chosen).map(FieldControl::Number)
                })
                .transpose()?,
        ),
        "boolean" => {
            if let Some(style) = &declared.style {
                return Err(table_error(format!(
                    "field {name}: a toggle has no style {style:?}"
                )));
            }
            (ParameterDescriptor::boolean(name), None)
        }
        "enum" => {
            let options = declared.options.clone().unwrap_or_default();
            if options.is_empty() || options.len() > MAX_OPTIONS {
                return Err(table_error(format!(
                    "field {name}: an enum field declares 1 to {MAX_OPTIONS} options"
                )));
            }
            (
                ParameterDescriptor::enumeration(name, options),
                declared
                    .style
                    .as_deref()
                    .map(|chosen| {
                        style::<ChoiceStyle>(name, "choice", chosen).map(FieldControl::Choice)
                    })
                    .transpose()?,
            )
        }
        "color" => (
            ParameterDescriptor::color(name),
            declared
                .style
                .as_deref()
                .map(|chosen| style::<ColorStyle>(name, "color", chosen).map(FieldControl::Color))
                .transpose()?,
        ),
        _ => return Err(unknown_kind()),
    };
    let mut parameter = parameter;
    if let Some(default) = &declared.default {
        parameter = parameter.default(default.clone());
    }
    if let Some(unit) = &declared.unit {
        if unit.chars().count() > MAX_UNIT_CHARS || unit.chars().any(char::is_control) {
            return Err(table_error(format!(
                "field {name}: unit {unit:?} is not a short unit"
            )));
        }
        parameter = parameter.unit(unit);
    }
    let hints: [(Option<f64>, NumberHint); 5] = [
        (declared.step, ParameterDescriptor::step),
        (declared.soft_min, ParameterDescriptor::soft_min),
        (declared.soft_max, ParameterDescriptor::soft_max),
        (declared.fine_step, ParameterDescriptor::fine_step),
        (declared.zero, ParameterDescriptor::zero),
    ];
    for (value, set) in hints {
        if let Some(value) = value {
            parameter = set(parameter, value);
        }
    }
    if let Some(precision) = declared.precision {
        parameter = parameter.precision(precision);
    }
    if let Some(notes) = &declared.notes {
        if notes.chars().count() > MAX_NOTES_CHARS {
            return Err(table_error(format!(
                "field {name}: notes are more than {MAX_NOTES_CHARS} characters"
            )));
        }
        parameter = parameter.notes(notes);
    }
    let mut field = Field::new(parameter, &declared.label);
    if let Some(history) = &declared.history {
        check_label(&format!("field {name}'s history"), history, MAX_LABEL_CHARS)?;
        field = field.history(history);
    }
    if let Some(control) = control {
        field = field.control(control);
    }
    if let Some(rail) = &declared.rail {
        field = field.rail(rail.clone());
    }
    Ok(field)
}

fn group(declared: &GroupDeclaration) -> Result<Group, Error> {
    let label = &declared.label;
    check_label("a group's label", label, MAX_LABEL_CHARS)?;
    if declared.wheels.len() > MAX_WHEELS {
        return Err(table_error(format!(
            "group {label} declares {} wheels, more than {MAX_WHEELS}",
            declared.wheels.len()
        )));
    }
    let group = declared
        .fields
        .iter()
        .fold(Group::new(label.clone(), []), |group, name| {
            group.field(name.clone())
        });
    Ok(declared.wheels.iter().fold(group, |group, wheel| {
        let built = Wheel::new(
            wheel.label.clone(),
            wheel.hue.clone(),
            wheel.saturation.clone(),
        );
        group.wheel(match &wheel.luminance {
            Some(luminance) => built.luminance(luminance.clone()),
            None => built,
        })
    }))
}
