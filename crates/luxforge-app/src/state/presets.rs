//! The Presets section's model: the preset library as `preset.list` last answered it, grouped in
//! the order the list returns, and the create form, whose checkboxes are derived from the
//! registered modules' field-patch groups. Nothing here calls the owner: the app layer loads the
//! library and owns the form's text, and this turns both into plain data the view draws.
//!
//! The section applies a library preset through the module's own `presets` control, so the one
//! request it can send is that control's action with the preset's settings, name and identity.
//! The parameter names are read from the action's descriptor by kind, and the only module-specific
//! rule here is the create form's white-balance default.
use crate::state::{
    Inputs,
    control_tree::walk,
    palette::PaletteAction,
    tools::{Rendered, classify, is_patch},
};
use luxforge_core::{
    ActionDescriptor, Control, ModuleDescriptor, ParameterKind, PresetSummary, ReportCounts,
    USER_PRESET_GROUP,
};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;

/// A group holding a field-patch parameter of one of these names is per-photo white balance, so
/// the create form leaves it unchecked: a look rarely means to carry one photograph's colour
/// temperature to the next.
const PER_PHOTO_FIELDS: [&str; 2] = ["temperature", "tint"];

/// The preset library as this desktop last read it. It is catalog data the owner holds; this is
/// the listing `preset.list` answered, replaced whole by every newer answer.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct PresetLibrary {
    /// Every preset, in the order `preset.list` returned them, or `None` before the first answer.
    pub(crate) presets: Option<Vec<PresetSummary>>,
    /// Why the last listing failed, until a newer one succeeds.
    pub(crate) error: Option<String>,
    /// Advanced by every adopted answer, so the section re-derives exactly when the library did.
    pub(crate) version: u64,
    /// The owner's event sequence the adopted listing was read at. Answers can complete out of
    /// order, so one read at an older sequence never replaces a newer one.
    pub(crate) sequence: u64,
    /// One of this desktop's own library requests is in flight. They run one at a time.
    pub(crate) pending: bool,
}

impl PresetLibrary {
    /// Adopt one `preset.list` answer read at `sequence`, unless a newer one is already shown.
    /// Returns whether it was adopted.
    pub(crate) fn adopt(&mut self, presets: Vec<PresetSummary>, sequence: u64) -> bool {
        if self.presets.is_some() && sequence < self.sequence {
            return false;
        }
        self.presets = Some(presets);
        self.sequence = sequence;
        self.error = None;
        self.version += 1;
        true
    }

    /// The listing failed. What was listed before stays on screen with the reason beside it.
    pub(crate) fn failed(&mut self, error: String) {
        self.error = Some(error);
        self.version += 1;
    }

    /// The first listing has answered, successfully or not, so a captured frame shows the library
    /// rather than its loading line.
    pub(crate) fn ready(&self) -> bool {
        self.presets.is_some() || self.error.is_some()
    }

    /// One listed preset, by its library identity.
    pub(crate) fn find(&self, id: &str) -> Option<&PresetSummary> {
        self.presets
            .as_deref()
            .unwrap_or_default()
            .iter()
            .find(|preset| preset.id.as_str() == id)
    }
}

/// The create form's own state: what is typed and which checkboxes the person changed.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct PresetForm {
    pub(crate) open: bool,
    pub(crate) name: String,
    pub(crate) group: String,
    /// The checkboxes the person changed, by label. Every other one keeps its derived default.
    pub(crate) checked: BTreeMap<String, bool>,
    /// The last create's refusal, shown in the form until the next attempt.
    pub(crate) error: Option<String>,
}

impl Default for PresetForm {
    fn default() -> Self {
        Self {
            open: false,
            name: String::new(),
            group: USER_PRESET_GROUP.into(),
            checked: BTreeMap::new(),
            error: None,
        }
    }
}

impl PresetForm {
    /// Whether this group's checkbox is on: the person's choice, else the group's default.
    pub(crate) fn is_checked(&self, group: &PresettableGroup) -> bool {
        self.checked
            .get(&group.label)
            .copied()
            .unwrap_or(group.default_checked)
    }
}

/// One checkbox of the create form: a group of controls whose action is a field patch, and the
/// parameters capturing it names, per action.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PresettableGroup {
    /// `<Module title> · <Group label>`, or the module title alone for the patch controls a module
    /// declares outside any group.
    pub(crate) label: String,
    /// The parameters this group captures, per field-patch action, in declaration order.
    pub(crate) fields: Vec<(String, Vec<String>)>,
    /// Checked unless the group carries per-photo white balance.
    pub(crate) default_checked: bool,
}

impl PresettableGroup {
    fn add(&mut self, action: &str, parameter: &str) {
        match self.fields.iter_mut().find(|(named, _)| named == action) {
            Some((_, parameters)) => {
                if !parameters.iter().any(|named| named == parameter) {
                    parameters.push(parameter.to_owned());
                }
            }
            None => self
                .fields
                .push((action.to_owned(), vec![parameter.to_owned()])),
        }
    }
}

/// Every presettable group the registered modules declare, in registry order: one per control
/// group whose value controls belong to a field-patch action, and one per module for such controls
/// declared outside any group. Unavailable modules offer none, and developer modules only when the
/// run lists them, exactly as their sections are listed.
pub(crate) fn presettable_groups(
    modules: &[ModuleDescriptor],
    developer: bool,
) -> Vec<PresettableGroup> {
    let mut groups = Vec::new();
    for module in modules
        .iter()
        .filter(|module| module.is_available())
        .filter(|module| developer || !module.developer)
    {
        let entries = collect(modules, module);
        groups.extend(
            entries
                .into_iter()
                .filter(|group: &PresettableGroup| !group.fields.is_empty()),
        );
    }
    for group in &mut groups {
        group.default_checked = !group.fields.iter().any(|(_, parameters)| {
            parameters
                .iter()
                .any(|parameter| PER_PHOTO_FIELDS.contains(&parameter.as_str()))
        });
    }
    groups
}

/// One module's entries, one per control group in declaration order. A value control belongs to
/// the entry of its enclosing group, or, at the module's top level, to the module's own entry,
/// which is added where the first such control is.
fn collect(modules: &[ModuleDescriptor], module: &ModuleDescriptor) -> Vec<PresettableGroup> {
    let mut entries = Vec::new();
    let mut loose = None;
    // Each group's path and its entry's index.
    let mut groups: Vec<(Vec<usize>, usize)> = Vec::new();
    let mut controls = walk(&module.controls);
    while let Some(control) = controls.next() {
        let fields: Vec<(&str, &str)> = match classify(control) {
            Rendered::Group { label, .. } => {
                entries.push(PresettableGroup {
                    label: format!("{} \u{00b7} {label}", module.title),
                    fields: Vec::new(),
                    default_checked: true,
                });
                groups.push((controls.path(), entries.len() - 1));
                continue;
            }
            Rendered::Number {
                action, parameter, ..
            }
            | Rendered::Toggle {
                action, parameter, ..
            }
            | Rendered::Choice {
                action, parameter, ..
            }
            | Rendered::Color {
                action, parameter, ..
            } => vec![(action, parameter)],
            Rendered::Curve {
                action, channels, ..
            } => channels
                .iter()
                .map(|channel| (action, channel.parameter.as_str()))
                .collect(),
            _ => Vec::new(),
        };
        if fields.is_empty() {
            continue;
        }
        let path = controls.path();
        let parent = &path[..path.len() - 1];
        let current = groups
            .iter()
            .find(|(group, _)| group == parent)
            .map(|(_, index)| *index);
        for (action, parameter) in fields {
            let declared = module
                .action(action)
                .is_some_and(|declared| declared.parameter(parameter).is_some());
            if !declared || !is_patch(modules, action) {
                continue;
            }
            let index = match current {
                Some(index) => index,
                None => *loose.get_or_insert_with(|| {
                    entries.push(PresettableGroup {
                        label: module.title.clone(),
                        fields: Vec::new(),
                        default_checked: true,
                    });
                    entries.len() - 1
                }),
            };
            entries[index].add(action, parameter);
        }
    }
    entries
}

/// The `fields` a `preset.capture` request names for the checked groups: each action's parameters
/// as an array, the union over every checked group in declaration order.
pub(crate) fn capture_fields(groups: &[PresettableGroup], form: &PresetForm) -> Map<String, Value> {
    let mut merged = PresettableGroup {
        label: String::new(),
        fields: Vec::new(),
        default_checked: true,
    };
    for group in groups.iter().filter(|group| form.is_checked(group)) {
        for (action, parameters) in &group.fields {
            for parameter in parameters {
                merged.add(action, parameter);
            }
        }
    }
    merged
        .fields
        .into_iter()
        .map(|(action, parameters)| {
            (
                action,
                Value::Array(parameters.into_iter().map(Value::from).collect()),
            )
        })
        .collect()
}

/// The module that declares the `presets` control and the action that control submits.
pub(crate) fn presets_control(modules: &[ModuleDescriptor]) -> Option<(&ModuleDescriptor, &str)> {
    modules.iter().find_map(|module| {
        walk(&module.controls).find_map(|control| match control {
            Control::Presets { action } => Some((module, action.as_str())),
            _ => None,
        })
    })
}

/// The fields a `presets` control's action carries for one library preset, under the names its
/// descriptor declares: the `settings` parameter, the required `string` (the name) and the
/// optional `string` (the library identity). Registration guarantees exactly those three.
pub(crate) fn apply_fields(
    action: &ActionDescriptor,
    preset: &PresetSummary,
) -> Option<Map<String, Value>> {
    let settings = action
        .parameters
        .iter()
        .find(|parameter| matches!(parameter.kind, ParameterKind::Settings))?;
    let name = action.parameters.iter().find(|parameter| {
        matches!(parameter.kind, ParameterKind::String { .. }) && parameter.required
    })?;
    let mut fields = Map::new();
    fields.insert(
        settings.name.clone(),
        Value::Object(preset.settings.clone()),
    );
    fields.insert(name.name.clone(), Value::from(preset.name.clone()));
    if let Some(id) = action.parameters.iter().find(|parameter| {
        matches!(parameter.kind, ParameterKind::String { .. }) && !parameter.required
    }) {
        fields.insert(id.name.clone(), Value::from(preset.id.as_str()));
    }
    Some(fields)
}

/// The report's four counts as the Partial badge's tooltip reads them.
pub(crate) fn counts_text(counts: &ReportCounts) -> String {
    counts.to_string()
}

/// The status line for one import: the preset's name and what happened to its settings. Neutral
/// settings are left out: they lose nothing, and the line is about what did or did not carry.
pub(crate) fn import_status(name: &str, counts: &ReportCounts) -> String {
    format!(
        "Imported \u{201c}{name}\u{201d}: {} mapped, {} unsupported, {} refused",
        counts.mapped, counts.unsupported, counts.refused
    )
}

/// Why a preset cannot apply in this build, or nothing when it can.
fn unavailable_reason(preset: &PresetSummary) -> Option<String> {
    match preset.unavailable.as_slice() {
        [] => None,
        [one] => Some(format!("Cannot apply: {one} is unavailable")),
        many => Some(format!("Cannot apply: {} are unavailable", many.join(", "))),
    }
}

/// One preset row.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PresetRow {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) group: String,
    /// Something in the imported file is not reproduced: a setting is unsupported or refused.
    pub(crate) partial: bool,
    /// The report's counts, for the Partial badge's tooltip.
    pub(crate) counts: Option<String>,
    /// Why the preset cannot apply here, shown under its name.
    pub(crate) unavailable: Option<String>,
    /// A click applies it now.
    pub(crate) enabled: bool,
    /// It came from a file, so it has an import report to copy.
    pub(crate) imported: bool,
    /// The fields a click submits with the section's action, when the preset can be applied at all.
    pub(crate) apply: Option<Map<String, Value>>,
}

/// One heading of the library and its rows, in the order the listing returned them.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PresetGroupModel {
    pub(crate) name: String,
    pub(crate) rows: Vec<PresetRow>,
}

/// One create-form checkbox.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PresetCheck {
    pub(crate) label: String,
    pub(crate) checked: bool,
    /// The parameters this checkbox captures, per action.
    pub(crate) fields: Vec<(String, Vec<String>)>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct PresetFormModel {
    pub(crate) open: bool,
    pub(crate) name: String,
    pub(crate) group: String,
    pub(crate) checks: Vec<PresetCheck>,
    pub(crate) error: Option<String>,
    pub(crate) can_create: bool,
}

/// The whole Presets section body.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct PresetsModel {
    /// The action a row submits.
    pub(crate) action: String,
    pub(crate) groups: Vec<PresetGroupModel>,
    /// The library answered and holds nothing.
    pub(crate) empty: bool,
    /// The library has not answered yet.
    pub(crate) loading: bool,
    /// Why the library could not be listed.
    pub(crate) error: Option<String>,
    pub(crate) form: PresetFormModel,
    pub(crate) can_import: bool,
    /// Why no row applies right now, when none does.
    pub(crate) apply_disabled: Option<String>,
}

impl PresetsModel {
    /// Every row, in listed order.
    pub(crate) fn rows(&self) -> impl Iterator<Item = &PresetRow> {
        self.groups.iter().flat_map(|group| group.rows.iter())
    }

    /// The section as a captured frame records it.
    pub(crate) fn summary(&self, expanded: bool) -> Value {
        json!({
            "expanded": expanded,
            "loading": self.loading,
            "error": self.error,
            "empty": self.empty,
            "apply_disabled": self.apply_disabled,
            "rows": self.rows().map(|row| json!({
                "id": row.id,
                "name": row.name,
                "group": row.group,
                "partial": row.partial,
                "counts": row.counts,
                "unavailable": row.unavailable,
                "enabled": row.enabled,
            })).collect::<Vec<_>>(),
            "form": {
                "open": self.form.open,
                "name": self.form.name,
                "group": self.form.group,
                "checked": self.form.checks.iter().filter(|check| check.checked)
                    .map(|check| check.label.clone()).collect::<Vec<_>>(),
                "error": self.form.error,
                "can_create": self.form.can_create,
            },
        })
    }
}

/// The section body for the module's `presets` control, which submits `action`. `enabled` and
/// `reason` are the section's own: a row applies only where every other edit could.
pub(crate) fn presets_model(
    action: &str,
    inputs: &Inputs<'_>,
    enabled: bool,
    reason: Option<&str>,
) -> PresetsModel {
    let library = inputs.presets;
    let declared = crate::state::tools::declared_action(inputs.modules, action);
    // One draft per client: a preset is a commit, so it waits for an open gesture or crop draft to
    // finish rather than displacing it.
    let apply_disabled = match (enabled, reason) {
        (false, Some(reason)) => Some(reason.to_owned()),
        (false, None) => Some("Presets cannot apply right now".to_owned()),
        (true, _) => inputs.preset_refusal.clone(),
    };
    let mut groups: Vec<PresetGroupModel> = Vec::new();
    for preset in library.presets.as_deref().unwrap_or_default() {
        let unavailable = unavailable_reason(preset);
        let apply = declared.and_then(|declared| apply_fields(declared, preset));
        let row = PresetRow {
            id: preset.id.as_str().to_owned(),
            name: preset.name.clone(),
            group: preset.group.clone(),
            partial: preset
                .report
                .is_some_and(|counts| counts.unsupported + counts.refused > 0),
            counts: preset.report.as_ref().map(counts_text),
            enabled: apply_disabled.is_none() && unavailable.is_none() && apply.is_some(),
            unavailable,
            imported: preset.report.is_some(),
            apply,
        };
        // The listing is sorted by group ignoring case, so one heading gathers a run of rows.
        match groups.last_mut() {
            Some(group) if group.name.to_lowercase() == preset.group.to_lowercase() => {
                group.rows.push(row)
            }
            _ => groups.push(PresetGroupModel {
                name: preset.group.clone(),
                rows: vec![row],
            }),
        }
    }
    let form = inputs.preset_form;
    let checks: Vec<PresetCheck> = presettable_groups(inputs.modules, inputs.developer)
        .into_iter()
        .map(|group| PresetCheck {
            checked: form.is_checked(&group),
            label: group.label,
            fields: group.fields,
        })
        .collect();
    let can_create = inputs.state.is_some()
        && inputs.display_entry.is_some()
        && !inputs.busy
        && !library.pending
        && !form.name.trim().is_empty()
        && !form.group.trim().is_empty()
        && checks.iter().any(|check| check.checked);
    PresetsModel {
        action: action.to_owned(),
        empty: library.presets.as_ref().is_some_and(Vec::is_empty),
        loading: !library.ready(),
        error: library.error.clone(),
        groups,
        form: PresetFormModel {
            open: form.open,
            name: form.name.clone(),
            group: form.group.clone(),
            checks,
            error: form.error.clone(),
            can_create,
        },
        can_import: !library.pending,
        apply_disabled,
    }
}

/// One `Apply preset: <name>` palette entry per library preset that can apply in this build, each
/// running exactly the message its row's click sends. The group follows the method in the detail,
/// so two presets of one name in different groups are told apart.
pub(crate) fn palette_entries(inputs: &Inputs<'_>) -> Vec<(String, String, PaletteAction)> {
    let Some((module, action)) = presets_control(inputs.modules) else {
        return Vec::new();
    };
    let Some(declared) = module
        .is_available()
        .then(|| module.action(action))
        .flatten()
    else {
        return Vec::new();
    };
    inputs
        .presets
        .presets
        .as_deref()
        .unwrap_or_default()
        .iter()
        .filter(|preset| preset.unavailable.is_empty())
        .filter_map(|preset| {
            let fields = apply_fields(declared, preset)?;
            Some((
                format!("Apply preset: {}", preset.name),
                format!("edit.{action} \u{00b7} {}", preset.group),
                PaletteAction::Run {
                    action: action.to_owned(),
                    preset: fields,
                },
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::testing::{descriptors, listed};

    fn labels(groups: &[PresettableGroup]) -> Vec<&str> {
        groups.iter().map(|group| group.label.as_str()).collect()
    }

    #[test]
    fn every_field_patch_group_is_one_checkbox_in_registry_order() {
        let modules = descriptors();
        let groups = presettable_groups(&modules, false);
        assert_eq!(
            labels(&groups),
            [
                "Basic \u{00b7} White balance",
                "Basic \u{00b7} Tone",
                "Basic \u{00b7} Colour",
                "Presence \u{00b7} Presence",
                "Colour mixer \u{00b7} Hue",
                "Colour mixer \u{00b7} Saturation",
                "Colour mixer \u{00b7} Luminance",
                "Vignette \u{00b7} Vignette",
            ],
            "RAW, transforms, crop and the pixel proof declare no field patch"
        );
        let tone = &groups[1];
        assert_eq!(
            tone.fields,
            [(
                "set-basic".to_owned(),
                [
                    "exposure",
                    "contrast",
                    "highlights",
                    "shadows",
                    "whites",
                    "blacks"
                ]
                .map(str::to_owned)
                .to_vec()
            )]
        );
        // Each checkbox carries only its own group's fields: the picker in White balance adds none.
        assert_eq!(
            groups[0].fields,
            [(
                "set-basic".to_owned(),
                vec!["temperature".to_owned(), "tint".to_owned()]
            )]
        );
        // The white-balance rule is the only default: every other group starts checked.
        let unchecked: Vec<_> = groups
            .iter()
            .filter(|group| !group.default_checked)
            .map(|group| group.label.as_str())
            .collect();
        assert_eq!(unchecked, ["Basic \u{00b7} White balance"]);
    }

    #[test]
    fn groups_come_from_descriptors_not_from_module_names() {
        // A module with patch controls outside any group gets one checkbox named for the module; a
        // group holding a `tint` field of a patch action is unchecked by default whatever its
        // module is called; a group of request inputs is no checkbox at all.
        let module = ModuleDescriptor::parse(&json!({
            "id":"fixture.look", "title":"Look", "effects":[],
            "actions":[
                {"id":"set-look","title":"Set look","notes":"patch","patch":true,"parameters":[
                    {"name":"amount","kind":"number","min":-1.0,"max":1.0,"default":0.0,"required":false,"notes":"n"},
                    {"name":"tint","kind":"number","min":-1.0,"max":1.0,"default":0.0,"required":false,"notes":"n"},
                    {"name":"glow","kind":"number","min":0.0,"max":1.0,"default":0.0,"required":false,"notes":"n"}]},
                {"id":"place","title":"Place","notes":"request","patch":false,"parameters":[
                    {"name":"x","kind":"number","min":0.0,"max":1.0,"default":0.0,"required":false,"notes":"n"}]}],
            "controls":[
                {"kind":"number","action":"set-look","parameter":"amount","label":"Amount"},
                {"kind":"group","label":"Cast","controls":[
                    {"kind":"number","action":"set-look","parameter":"tint","label":"Tint"}]},
                {"kind":"group","label":"Where","controls":[
                    {"kind":"number","action":"place","parameter":"x","label":"X"}]},
                {"kind":"number","action":"set-look","parameter":"glow","label":"Glow"}],
            "availability":{"kind":"available"}
        }))
        .expect("a valid fixture descriptor");
        let groups = presettable_groups(std::slice::from_ref(&module), false);
        assert_eq!(labels(&groups), ["Look", "Look \u{00b7} Cast"]);
        assert_eq!(
            groups[0].fields,
            [(
                "set-look".to_owned(),
                vec!["amount".to_owned(), "glow".to_owned()]
            )]
        );
        assert!(groups[0].default_checked);
        assert!(!groups[1].default_checked, "a tint field is per-photo");
        // An unavailable module offers nothing to capture.
        let unavailable = ModuleDescriptor {
            availability: luxforge_core::Availability::Unavailable {
                reason: "test".into(),
            },
            ..module
        };
        assert!(presettable_groups(&[unavailable], false).is_empty());
    }

    #[test]
    fn the_checked_groups_become_exactly_the_capture_fields() {
        let modules = descriptors();
        let groups = presettable_groups(&modules, false);
        let mut form = PresetForm::default();
        // By default everything but white balance.
        let fields = capture_fields(&groups, &form);
        assert_eq!(
            fields["set-basic"],
            json!([
                "exposure",
                "contrast",
                "highlights",
                "shadows",
                "whites",
                "blacks",
                "vibrance",
                "saturation"
            ])
        );
        assert!(fields.contains_key("set-presence"));
        assert!(fields.contains_key("set-mixer"));
        assert!(fields.contains_key("set-vignette"));
        // Tone alone, as the smoke scenario creates it.
        for group in &groups {
            form.checked
                .insert(group.label.clone(), group.label == "Basic \u{00b7} Tone");
        }
        assert_eq!(
            Value::Object(capture_fields(&groups, &form)),
            json!({"set-basic": ["exposure", "contrast", "highlights", "shadows", "whites", "blacks"]})
        );
        // Checking white balance adds its two fields to the same action, in declaration order.
        form.checked
            .insert("Basic \u{00b7} White balance".into(), true);
        assert_eq!(
            capture_fields(&groups, &form)["set-basic"],
            json!([
                "temperature",
                "tint",
                "exposure",
                "contrast",
                "highlights",
                "shadows",
                "whites",
                "blacks"
            ])
        );
    }

    #[test]
    fn a_rows_click_carries_the_settings_name_and_identity_under_the_declared_names() {
        let modules = descriptors();
        let (module, action) = presets_control(&modules).expect("the presets control");
        assert_eq!(module.id, "luxforge.presets");
        let declared = module.action(action).expect("its action");
        let preset = listed("Warm", "Looks", None);
        let fields = apply_fields(declared, &preset).expect("a complete request");
        assert_eq!(
            Value::Object(fields),
            json!({
                "settings": {"set-basic": {"exposure": 0.5}},
                "name": "Warm",
                "preset-id": preset.id.as_str(),
            })
        );
    }

    #[test]
    fn import_status_names_the_preset_and_what_did_not_carry() {
        let counts = ReportCounts {
            mapped: 18,
            neutral: 4,
            unsupported: 2,
            refused: 1,
        };
        assert_eq!(
            import_status("Soft film", &counts),
            "Imported \u{201c}Soft film\u{201d}: 18 mapped, 2 unsupported, 1 refused"
        );
        assert_eq!(
            counts_text(&counts),
            "18 mapped, 4 neutral, 2 unsupported, 1 refused"
        );
    }
}
