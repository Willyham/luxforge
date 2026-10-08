//! The settings groups: what a preset's create form and Copy settings offer to carry from one
//! photograph to others, derived from the registered modules' control descriptors, and what each
//! group holds on one photograph. `preset.groups` answers it, `preset.capture {groups}` reads it,
//! and the desktop draws its checkboxes from it, so every client offers the same groups under the
//! same identities with the same defaults, refusals and per-kind skips.
//!
//! A group is one control group whose value controls belong to a presettable field patch, or a
//! module's patch controls declared outside any group. Its identity is the module's and the
//! group's label (`luxforge.basic/tone`), or the module's alone for its loose controls. A group's
//! request (`fields`) names controls as the photo's section shows them; capture resolves it per
//! source kind exactly as [`EditorService::capture_preset`] does, and what a set captured on one
//! kind loses on another is the composite plan's own skip rule: a step whose module does not apply
//! to the target's kind, and a field a control variant supersedes there.
//!
//! Everything here walks descriptors: `O(modules × controls)` with no stack read, except the
//! per-photo answer, which reads one entry's stored payloads once and captures each group from
//! them: no source is opened and nothing is rendered or sampled.
use crate::{
    ActionDescriptor, AssetId, Control, EditorService, EntryId, Error, ModuleDescriptor, SourceTag,
    modules::{
        Superseded, not_applicable, settings_action_in, superseded_in, superseded_refusal_in,
    },
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

/// The most group and analysis identities one `preset.capture {groups}` names.
pub(crate) const MAX_CAPTURE_GROUPS: usize = 64;

/// What `preset.groups` answers: every settings group and analysis step the registry declares, in
/// registry order, and, when a photograph was named, that photograph and entry.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettingsGroups {
    pub groups: Vec<SettingsGroup>,
    pub analysis: Vec<SettingsAnalysis>,
    /// The photograph whose entry the groups' `state` and `reason` describe.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub photo: Option<SettingsPhoto>,
}

/// The photograph and entry a per-photo answer read.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettingsPhoto {
    pub asset_id: AssetId,
    pub entry_id: EntryId,
    pub kind: SourceTag,
}

/// One group of presettable controls.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettingsGroup {
    /// `<module id>/<group label as a slug>`, or the module's id for its loose controls.
    pub id: String,
    pub module: String,
    pub module_title: String,
    /// The group's own label, or the module's title for its loose controls.
    pub label: String,
    /// `<module title> · <label>`, or the module's title alone for its loose controls.
    pub title: String,
    /// The fields capture names for the group, per field-patch action, in declaration order: a
    /// `preset.capture` `fields` value.
    pub fields: BTreeMap<String, Vec<String>>,
    /// The module declares the group's values as usually belonging to one photograph
    /// ([`crate::GroupControl::per_photo`]), as white balance's do.
    pub per_photo: bool,
    /// Whether a create form or Copy checks the group before a person chooses: every group but a
    /// per-photo one.
    pub default_checked: bool,
    /// Why no settings set may carry the group in this registry (`incompatible: unavailable module
    /// luxforge.presence`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unavailable: Option<String>,
    /// The group on a photo of each source kind, in [`SourceTag::ALL`] order.
    pub kinds: Vec<GroupOnKind>,
    /// The analysis steps that overwrite a field of the group, so a set cannot carry both.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub overwritten_by: Vec<String>,
    /// On the named photograph's entry: whether the group differs from its declared defaults, or
    /// cannot be captured there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<GroupState>,
    /// Why the group cannot be captured on the named photograph's entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// A group on a photograph's entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GroupState {
    /// A captured field differs from its declared default.
    Custom,
    /// Every captured field is at its declared default.
    Original,
    /// The group cannot be captured from this entry; `reason` says why.
    Refused,
}

/// One group on a photo of one source kind.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroupOnKind {
    pub kind: SourceTag,
    /// What capture reads for the group on a photo of this kind, as a `preset.capture` `fields`
    /// value: an array of fields, or `true` for an action a control variant provides there, which
    /// is captured whole (Basic's White balance is `{"set-raw": true}` on a RAW photo).
    pub captures: Map<String, Value>,
    /// Why capture refuses the group on a photo of this kind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refused: Option<String>,
    /// The target kinds on which a set captured here skips some or all of the group, as applying
    /// it skips them: the reasons are the skips' own.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skipped: Vec<KindSkip>,
}

/// What applying a group captured on one kind skips on a photo of another.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KindSkip {
    pub kind: SourceTag,
    /// Every field of the group is skipped there, so the target keeps its own.
    pub all: bool,
    pub reasons: Vec<String>,
}

/// One declared analysis step a settings set may carry, which recomputes its fields on each photo.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettingsAnalysis {
    /// The action's identity, which `preset.capture` names with `true`.
    pub id: String,
    pub module: String,
    pub module_title: String,
    /// The action's title.
    pub label: String,
    /// `<module title> · <label>`.
    pub title: String,
    /// The fields the step sets, per field-patch action, as its descriptor declares.
    pub writes: BTreeMap<String, Vec<String>>,
    /// The groups holding a field it writes, which a set carrying the step cannot carry too.
    pub overwrites: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unavailable: Option<String>,
    /// Why the step cannot be captured on the named photograph.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl SettingsGroups {
    /// One group by its identity.
    pub fn group(&self, id: &str) -> Option<&SettingsGroup> {
        self.groups.iter().find(|group| group.id == id)
    }

    /// One analysis step by its identity.
    pub fn analysis_step(&self, id: &str) -> Option<&SettingsAnalysis> {
        self.analysis.iter().find(|step| step.id == id)
    }

    /// The `preset.capture` `fields` these group and analysis identities name: each group's fields
    /// merged per action in the order named, and `true` for each analysis step. An unknown or
    /// repeated identity is `validation`.
    pub fn capture_fields(&self, ids: &[impl AsRef<str>]) -> Result<Map<String, Value>, Error> {
        if ids.is_empty() || ids.len() > MAX_CAPTURE_GROUPS {
            return Err(Error::validation(format!(
                "groups names 1 to {MAX_CAPTURE_GROUPS} groups; this one names {}",
                ids.len()
            )));
        }
        let mut fields = Map::new();
        let mut seen = Vec::with_capacity(ids.len());
        for id in ids.iter().map(AsRef::as_ref) {
            if seen.contains(&id) {
                return Err(Error::validation(format!("groups names {id} twice")));
            }
            seen.push(id);
            if let Some(group) = self.group(id) {
                for (action, names) in &group.fields {
                    let entry = fields
                        .entry(action.clone())
                        .or_insert_with(|| Value::Array(Vec::new()));
                    if let Value::Array(merged) = entry {
                        for name in names {
                            if !merged.iter().any(|held| held == name.as_str()) {
                                merged.push(Value::from(name.as_str()));
                            }
                        }
                    }
                }
            } else if self.analysis_step(id).is_some() {
                fields.insert(id.to_owned(), Value::Bool(true));
            } else {
                return Err(Error::validation(format!("unknown settings group {id}")));
            }
        }
        Ok(fields)
    }
}

impl SettingsGroup {
    /// The group on a photo of `kind`.
    pub fn on(&self, kind: SourceTag) -> Option<&GroupOnKind> {
        self.kinds.iter().find(|on| on.kind == kind)
    }
}

/// Every settings group and analysis step `modules` declare, in their order: what `preset.groups`
/// answers without a photograph, from the registry's descriptors, and what a client derives from
/// its `module.list` alike. An unavailable module's groups are listed with the refusal capture
/// would give them. `O(modules × controls)`; reads no stack.
pub fn settings_groups<'a>(
    modules: impl IntoIterator<Item = &'a ModuleDescriptor>,
) -> SettingsGroups {
    let modules: Vec<&ModuleDescriptor> = modules.into_iter().collect();
    let superseded = superseded_in(&modules);
    let mut groups = Vec::new();
    for module in &modules {
        let mut drafts = Vec::new();
        let mut loose = None;
        collect(module, &module.controls, None, &mut drafts, &mut loose);
        let mut ids: Vec<String> = Vec::new();
        for draft in drafts.into_iter().filter(|draft| !draft.fields.is_empty()) {
            let base = match draft.label {
                Some(label) => format!("{}/{}", module.id, slug(label)),
                None => module.id.clone(),
            };
            let mut id = base.clone();
            let mut ordinal = 1;
            while ids.contains(&id) {
                ordinal += 1;
                id = format!("{base}-{ordinal}");
            }
            ids.push(id.clone());
            let label = draft.label.unwrap_or(&module.title).to_owned();
            let title = match draft.label {
                Some(_) => format!("{} \u{00b7} {label}", module.title),
                None => label.clone(),
            };
            let mut fields: BTreeMap<String, Vec<String>> = BTreeMap::new();
            for (action, name) in draft.fields {
                let names = fields.entry(action.to_owned()).or_default();
                if !names.iter().any(|held| held == name) {
                    names.push(name.to_owned());
                }
            }
            groups.push(SettingsGroup {
                id,
                module: module.id.clone(),
                module_title: module.title.clone(),
                label,
                title,
                kinds: SourceTag::ALL
                    .into_iter()
                    .map(|kind| on_kind(&modules, &superseded, &fields, kind))
                    .collect(),
                unavailable: module
                    .check_available()
                    .err()
                    .map(|error| error.to_string()),
                fields,
                per_photo: draft.per_photo,
                default_checked: !draft.per_photo,
                overwritten_by: Vec::new(),
                state: None,
                reason: None,
            });
        }
    }
    let mut analysis = Vec::new();
    for module in &modules {
        for action in module
            .actions
            .iter()
            .filter(|action| action.analysis.is_some() && action.preset && !action.patch)
        {
            let writes = action
                .analysis
                .as_ref()
                .map(|analysis| analysis.writes.clone())
                .unwrap_or_default();
            let overwrites = groups
                .iter_mut()
                .filter(|group| overlaps(&group.fields, &writes))
                .map(|group| {
                    group.overwritten_by.push(action.id.clone());
                    group.id.clone()
                })
                .collect();
            analysis.push(SettingsAnalysis {
                id: action.id.clone(),
                module: module.id.clone(),
                module_title: module.title.clone(),
                label: action.title.clone(),
                title: format!("{} \u{00b7} {}", module.title, action.title),
                writes,
                overwrites,
                unavailable: module
                    .check_available()
                    .err()
                    .map(|error| error.to_string()),
                reason: None,
            });
        }
    }
    SettingsGroups {
        groups,
        analysis,
        photo: None,
    }
}

/// Whether a group's fields and an analysis step's writes share a field.
fn overlaps(
    fields: &BTreeMap<String, Vec<String>>,
    writes: &BTreeMap<String, Vec<String>>,
) -> bool {
    fields.iter().any(|(action, names)| {
        writes
            .get(action)
            .is_some_and(|written| names.iter().any(|name| written.contains(name)))
    })
}

/// A group as the walk gathers it: its label (`None` for a module's loose controls), whether it is
/// per photo, and its `(action, field)` pairs in declaration order.
struct Draft<'d> {
    label: Option<&'d str>,
    per_photo: bool,
    fields: Vec<(&'d str, &'d str)>,
}

/// One module's groups, one per control group in declaration order. A value control belongs to
/// the group that encloses it, or, at the module's top level, to the module's own loose group,
/// which is placed where the first such control is. A field counts when its action is a
/// presettable field patch the same module declares, with that parameter.
fn collect<'d>(
    module: &'d ModuleDescriptor,
    controls: &'d [Control],
    group: Option<usize>,
    drafts: &mut Vec<Draft<'d>>,
    loose: &mut Option<usize>,
) {
    for control in controls {
        let fields: Vec<(&str, &str)> = match control {
            Control::Group(declared) => {
                drafts.push(Draft {
                    label: Some(&declared.label),
                    per_photo: declared.per_photo,
                    fields: Vec::new(),
                });
                let index = drafts.len() - 1;
                collect(module, &declared.controls, Some(index), drafts, loose);
                continue;
            }
            Control::Number(number) => vec![(&number.action, &number.parameter)],
            Control::Toggle(toggle) => vec![(&toggle.action, &toggle.parameter)],
            Control::Choice(choice) => vec![(&choice.action, &choice.parameter)],
            Control::Color(color) => vec![(&color.action, &color.parameter)],
            Control::Curve(curve) => curve
                .channels
                .iter()
                .map(|channel| (curve.action.as_str(), channel.parameter.as_str()))
                .collect(),
            // A band's fields are presettable through the number controls that declare them, and
            // no other kind carries a field.
            Control::Range(_)
            | Control::Action(_)
            | Control::Picker(_)
            | Control::Task(_)
            | Control::Presets(_)
            | Control::QueryChoice(_) => Vec::new(),
        };
        for (action, parameter) in fields {
            let presettable = module.action(action).is_some_and(|declared| {
                declared.patch && declared.preset && declared.parameter(parameter).is_some()
            });
            if !presettable {
                continue;
            }
            let index = match group {
                Some(index) => index,
                None => *loose.get_or_insert_with(|| {
                    drafts.push(Draft {
                        label: None,
                        per_photo: false,
                        fields: Vec::new(),
                    });
                    drafts.len() - 1
                }),
            };
            drafts[index].fields.push((action, parameter));
        }
    }
}

/// A group label as an identity segment: ASCII letters and digits lowercased, every other run of
/// characters one `-`.
fn slug(label: &str) -> String {
    let mut slug = String::with_capacity(label.len());
    for character in label.chars() {
        if character.is_ascii_alphanumeric() {
            slug.push(character.to_ascii_lowercase());
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    if slug.is_empty() {
        slug.push_str("group");
    }
    slug
}

/// A group's request resolved for a photo of `kind`, as capture resolves it
/// ([`EditorService::capture_preset`]): each named action must be presettable and apply to the
/// kind, and a field a control variant supersedes there is captured as the variant's action,
/// whole. Then what a set captured there skips on each other kind, as the composite plan skips
/// it: a step whose module does not apply to the target, and a field superseded on it.
fn on_kind(
    modules: &[&ModuleDescriptor],
    superseded: &[Superseded<'_>],
    fields: &BTreeMap<String, Vec<String>>,
    kind: SourceTag,
) -> GroupOnKind {
    let refusal = |action: &str| -> Option<String> {
        match settings_action_in(modules, action) {
            Ok((module, _)) => module.check_applies_to(kind).err(),
            Err(error) => Some(error),
        }
        .map(|error| error.to_string())
    };
    let mut captures = Map::new();
    let mut refused = None;
    for (action, names) in fields {
        if let Some(reason) = refusal(action) {
            refused.get_or_insert(reason);
            continue;
        }
        let mut kept = Vec::new();
        for name in names {
            match superseded.iter().find(|field| {
                field.source == kind && field.action == action && field.parameter == name
            }) {
                Some(field) => {
                    if let Some(reason) = refusal(field.by_action) {
                        refused.get_or_insert(reason);
                    }
                    captures.insert(field.by_action.to_owned(), Value::Bool(true));
                }
                None => kept.push(Value::from(name.as_str())),
            }
        }
        if !kept.is_empty() && !captures.contains_key(action) {
            captures.insert(action.clone(), Value::Array(kept));
        }
    }
    let skipped = SourceTag::ALL
        .into_iter()
        .filter(|target| *target != kind)
        .filter_map(|target| skipped_on(modules, superseded, &captures, target))
        .collect();
    GroupOnKind {
        kind,
        captures,
        refused,
        skipped,
    }
}

/// What applying `captures` to a photo of `target` skips, by the composite plan's rule, or nothing
/// when every field applies.
fn skipped_on(
    modules: &[&ModuleDescriptor],
    superseded: &[Superseded<'_>],
    captures: &Map<String, Value>,
    target: SourceTag,
) -> Option<KindSkip> {
    let mut total = 0;
    let mut skipped = 0;
    let mut reasons: Vec<String> = Vec::new();
    let mut reason = |text: String| {
        if !reasons.contains(&text) {
            reasons.push(text);
        }
    };
    for (action_id, requested) in captures {
        let Some((module, action)) = modules
            .iter()
            .find_map(|module| module.action(action_id).map(|action| (*module, action)))
        else {
            continue;
        };
        let names = requested_names(action, requested);
        total += names.len();
        if !module.applies_to(target) {
            skipped += names.len();
            reason(not_applicable(&module.title, target));
            continue;
        }
        for name in names {
            if let Some(field) = superseded.iter().find(|field| {
                field.source == target && field.action == action_id && field.parameter == name
            }) {
                skipped += 1;
                reason(superseded_refusal_in(modules, field));
            }
        }
    }
    (skipped > 0).then_some(KindSkip {
        kind: target,
        all: skipped == total,
        reasons,
    })
}

/// The fields a capture request names for one action: its array, or every declared parameter for
/// `true`.
fn requested_names<'a>(action: &'a ActionDescriptor, requested: &'a Value) -> Vec<&'a str> {
    match requested {
        Value::Array(names) => names.iter().filter_map(Value::as_str).collect(),
        _ => action
            .parameters
            .iter()
            .map(|parameter| parameter.name.as_str())
            .collect(),
    }
}

/// Two values the same setting, as a client compares a captured value with its default: numbers
/// by value, whatever their JSON spelling, and arrays and objects element by element.
fn same_value(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Number(a), Value::Number(b)) => a.as_f64() == b.as_f64(),
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| same_value(a, b))
        }
        (Value::Object(a), Value::Object(b)) => {
            a.len() == b.len()
                && a.iter()
                    .all(|(key, a)| b.get(key).is_some_and(|b| same_value(a, b)))
        }
        _ => left == right,
    }
}

impl EditorService {
    /// `preset.groups`: the registry's settings groups ([`settings_groups`]) and, for a
    /// photograph's entry, each group's state there: captured exactly as `preset.capture` with the
    /// group's `fields` would capture it, then Custom when a captured field differs from its
    /// declared default and Original when none does, or Refused with capture's own refusal. An
    /// analysis step is refused there when capture would refuse naming it.
    ///
    /// One read of the asset and the entry, then one capture per group from the stored payloads:
    /// `O(groups × layers)`, with no source opened and nothing rendered.
    pub(crate) fn preset_groups(
        &self,
        photo: Option<(&AssetId, &EntryId)>,
    ) -> Result<SettingsGroups, Error> {
        let registry = self.registry();
        let mut answer = settings_groups(registry.descriptors());
        let Some((asset_id, entry_id)) = photo else {
            return Ok(answer);
        };
        let kind = self.state(asset_id)?.asset.source.tag();
        let entry = self.entry(asset_id, entry_id)?;
        let layers = &entry.snapshot.recipe.layers;
        for group in &mut answer.groups {
            let request: Map<String, Value> = group
                .fields
                .iter()
                .map(|(action, names)| {
                    (
                        action.clone(),
                        Value::Array(
                            names
                                .iter()
                                .map(|name| Value::from(name.as_str()))
                                .collect(),
                        ),
                    )
                })
                .collect();
            match self.capture_from(kind, layers, &request) {
                Ok(settings) => {
                    group.state = Some(if custom(registry, &settings) {
                        GroupState::Custom
                    } else {
                        GroupState::Original
                    });
                }
                Err(error) => {
                    group.state = Some(GroupState::Refused);
                    group.reason = Some(error.to_string());
                }
            }
        }
        for step in &mut answer.analysis {
            step.reason = self
                .capture_action(registry, &step.id, kind)
                .err()
                .map(|error| error.to_string());
        }
        answer.photo = Some(SettingsPhoto {
            asset_id: asset_id.clone(),
            entry_id: entry_id.clone(),
            kind,
        });
        Ok(answer)
    }
}

/// Whether a captured set differs from its declared defaults: a field with no default, or one the
/// action does not declare, counts as set.
fn custom(registry: &crate::ModuleRegistry, settings: &Map<String, Value>) -> bool {
    settings.iter().any(|(action_id, values)| {
        let declared = registry.action(action_id).map(|(_, action)| action);
        values.as_object().is_some_and(|values| {
            values.iter().any(|(name, value)| {
                declared
                    .and_then(|action| action.parameter(name))
                    .and_then(|parameter| parameter.default.as_ref())
                    .is_none_or(|default| !same_value(default, value))
            })
        })
    })
}
