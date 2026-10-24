//! The Presets section's model: the preset library as `preset.list` last answered it, grouped in
//! the order the list returns, and the create form, whose checkboxes are the core's settings
//! groups and analysis steps (`preset.groups`). Nothing here calls the owner: the app layer loads
//! the library and owns the form's text, and this turns both into plain data the view draws.
//!
//! The section applies a library preset through the module's own `presets` control, so the one
//! request it can send is that control's action with the preset's settings, name and identity.
//! The parameter names are read from the action's descriptor by kind. Which groups exist, their
//! defaults and which an analysis step overwrites are the core's ([`luxforge_core::settings_groups`]),
//! so no module-specific rule lives here.
use crate::state::{Inputs, control_tree::walk, palette::PaletteAction, tools::ValueEdit};
use luxforge_core::{
    ActionDescriptor, Control, ModuleDescriptor, ParameterKind, PresetSummary, ReportCounts,
    SettingsGroup, SettingsGroups, SettingsOrigin, USER_PRESET_GROUP,
};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};

/// The preset library as this desktop last read it. It is catalog data the owner holds; this is
/// the listing `preset.list` answered, replaced whole by every newer answer.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct PresetLibrary {
    pub(crate) interaction: PresetInteraction,
    /// Every preset, in the order `preset.list` returned them, or `None` before the first answer.
    pub(crate) presets: Option<Vec<PresetSummary>>,
    /// Why the last listing failed, until a newer one succeeds.
    pub(crate) error: Option<String>,
    /// The owner's event sequence the adopted listing was read at. Answers can complete out of
    /// order, so one read at an older sequence never replaces a newer one.
    pub(crate) sequence: u64,
    /// One of this desktop's own library requests is in flight. They run one at a time.
    pub(crate) pending: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PresetInteraction {
    pub(crate) amount: f64,
    pub(crate) hovered: Option<String>,
    pub(crate) editing: ValueEdit,
    pub(crate) invalid: Option<String>,
}

impl Default for PresetInteraction {
    fn default() -> Self {
        Self {
            amount: 100.0,
            hovered: None,
            editing: ValueEdit::None,
            invalid: None,
        }
    }
}

/// The selected preset is provenance in the displayed current entry, never a sticky photo-wide
/// flag. An intervening edit, navigation, deletion or library settings change clears it naturally.
pub(crate) fn selected_preset<'a>(
    library: &'a PresetLibrary,
    entry: &luxforge_core::HistoryEntry,
) -> Option<&'a PresetSummary> {
    if entry.action_id != "apply-settings" || entry.parameters["origin"]["kind"] != "preset" {
        return None;
    }
    let id = entry.parameters["origin"]["preset_id"].as_str()?;
    library
        .find(id)
        .filter(|preset| entry.parameters["settings"] == Value::Object(preset.settings.clone()))
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
        true
    }

    /// The listing failed. What was listed before stays on screen with the reason beside it.
    pub(crate) fn failed(&mut self, error: String) {
        self.error = Some(error);
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
    /// The checkboxes the person changed, by settings-group identity. Every other one keeps the
    /// group's `default_checked`.
    pub(crate) checked: BTreeMap<String, bool>,
    /// The analysis steps checked, by identity: each recomputes its fields on every photo the
    /// preset is applied to, so the groups it overwrites are cleared and disabled.
    pub(crate) analysis: BTreeSet<String>,
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
            analysis: BTreeSet::new(),
            error: None,
        }
    }
}

impl PresetForm {
    /// Whether this group's checkbox is on: never while a checked analysis step overwrites it,
    /// else the person's choice, else the group's default.
    pub(crate) fn is_checked(&self, group: &SettingsGroup) -> bool {
        !self.overwritten(group)
            && self
                .checked
                .get(&group.id)
                .copied()
                .unwrap_or(group.default_checked)
    }

    /// A checked analysis step overwrites a field of this group, so the form cannot carry both.
    pub(crate) fn overwritten(&self, group: &SettingsGroup) -> bool {
        group
            .overwritten_by
            .iter()
            .any(|step| self.analysis.contains(step))
    }

    /// The identities a `preset.capture {groups}` request names for this form: every checked group
    /// the registry can capture, in registry order, then every checked analysis step it offers.
    pub(crate) fn capture_ids(&self, groups: &SettingsGroups) -> Vec<String> {
        groups
            .groups
            .iter()
            .filter(|group| group.unavailable.is_none() && self.is_checked(group))
            .map(|group| group.id.clone())
            .chain(
                groups
                    .analysis
                    .iter()
                    .filter(|step| step.unavailable.is_none() && self.analysis.contains(&step.id))
                    .map(|step| step.id.clone()),
            )
            .collect()
    }
}

/// The settings groups and analysis steps this desktop offers, from the core's one derivation over
/// the listed modules, which `preset.groups` answers too. Developer modules count only when the run
/// lists them, exactly as their sections are listed.
pub(crate) fn settings_groups(modules: &[ModuleDescriptor], developer: bool) -> SettingsGroups {
    luxforge_core::settings_groups(
        modules
            .iter()
            .filter(|module| developer || !module.developer),
    )
}

/// The module that declares the `presets` control and the action that control submits.
pub(crate) fn presets_control(modules: &[ModuleDescriptor]) -> Option<(&ModuleDescriptor, &str)> {
    modules.iter().find_map(|module| {
        walk(&module.controls).find_map(|control| match control {
            Control::Presets(luxforge_core::PresetsControl { action }) => {
                Some((module, action.as_str()))
            }
            _ => None,
        })
    })
}

/// The fields a `presets` control's action carries for one library preset, under the names its
/// descriptor declares: the `settings` parameter and the `settings-origin` one, the preset's name and
/// library identity. Amount is optional and is added by the section model when supported.
pub(crate) fn apply_fields(
    action: &ActionDescriptor,
    preset: &PresetSummary,
) -> Option<Map<String, Value>> {
    let named = |kind: fn(&ParameterKind) -> bool| {
        action
            .parameters
            .iter()
            .find(|parameter| kind(&parameter.kind))
            .map(|parameter| parameter.name.clone())
    };
    let settings = named(|kind| matches!(kind, ParameterKind::Settings))?;
    let origin = named(|kind| matches!(kind, ParameterKind::SettingsOrigin))?;
    let mut fields = Map::new();
    fields.insert(settings, Value::Object(preset.settings.clone()));
    let origin_value = SettingsOrigin::Preset {
        name: preset.name.clone(),
        preset_id: Some(preset.id.clone()),
    };
    fields.insert(origin, serde_json::to_value(origin_value).ok()?);
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
    pub(crate) selected: bool,
    pub(crate) preview_unavailable: Option<String>,
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

/// One create-form checkbox: a settings group, by its identity and its title.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PresetCheck {
    pub(crate) id: String,
    pub(crate) enabled: bool,
    pub(crate) label: String,
    pub(crate) checked: bool,
}

/// One analysis step the create form offers, drawn before the first group it overwrites
/// (`before`), or after every group when it overwrites none.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AnalysisCheck {
    pub(crate) id: String,
    /// `<Module> · <step> (per photo)`: the step recomputes on every photo the preset reaches.
    pub(crate) label: String,
    pub(crate) checked: bool,
    pub(crate) before: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct PresetFormModel {
    pub(crate) analysis: Vec<AnalysisCheck>,
    pub(crate) enabled: bool,
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
    pub(crate) amount: f64,
    pub(crate) amount_enabled: bool,
    pub(crate) amount_reason: Option<String>,
    pub(crate) amount_edit: ValueEdit,
    pub(crate) amount_invalid: Option<String>,
    pub(crate) amount_dragging: bool,
    pub(crate) selected: Option<String>,
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
    pub(crate) can_manage: bool,
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
            "can_manage": self.can_manage,
            "can_import": self.can_import,
            "apply_disabled": self.apply_disabled,
            "amount": self.amount,
            "amount_enabled": self.amount_enabled,
            "amount_reason": self.amount_reason,
            "selected": self.selected,
            "rows": self.rows().map(|row| json!({
                "id": row.id,
                "name": row.name,
                "group": row.group,
                "partial": row.partial,
                "counts": row.counts,
                "unavailable": row.unavailable,
                "enabled": row.enabled,
                "selected": row.selected,
                "preview_unavailable": row.preview_unavailable,
            })).collect::<Vec<_>>(),
            "form": {
                "analysis": self.form.analysis.iter().filter(|step| step.checked).map(|step| &step.id).collect::<Vec<_>>(),
                "disabled": self.form.checks.iter().filter(|check| !check.enabled).map(|check| &check.label).collect::<Vec<_>>(),
                "enabled": self.form.enabled,
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
    let can_manage = !inputs.mask_tool_owns_controls();
    let library = inputs.presets;
    let declared = crate::state::tools::declared_action(inputs.modules, action);
    let selected = inputs
        .document
        .state
        .as_ref()
        .and_then(|state| selected_preset(library, &state.current_entry));
    let amount_target = library
        .interaction
        .hovered
        .as_deref()
        .and_then(|id| library.find(id))
        .or(selected);
    let amount_reason = amount_target
        .and_then(|preset| {
            luxforge_core::settings_at_amount(inputs.modules, &preset.settings, 0.0).err()
        })
        .map(|error| error.detail);
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
        let supports_amount =
            luxforge_core::settings_at_amount(inputs.modules, &preset.settings, 0.0).is_ok();
        let mut apply = declared.and_then(|declared| apply_fields(declared, preset));
        if supports_amount
            && library.interaction.amount != 100.0
            && let Some(fields) = &mut apply
        {
            fields.insert("amount".into(), json!(library.interaction.amount));
        }
        let row = PresetRow {
            selected: selected.is_some_and(|selected| selected.id == preset.id),
            preview_unavailable: (!luxforge_core::settings_can_preview(
                inputs.modules,
                &preset.settings,
            ))
            .then(|| "Hover preview unavailable: this preset contains analysis steps".into()),
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
    // A preset carries only what this registry can capture, so an unavailable module's groups and
    // steps are not offered.
    let offered = settings_groups(inputs.modules, inputs.developer);
    let checks: Vec<PresetCheck> = offered
        .groups
        .iter()
        .filter(|group| group.unavailable.is_none())
        .map(|group| PresetCheck {
            id: group.id.clone(),
            enabled: !form.overwritten(group),
            checked: form.is_checked(group),
            label: group.title.clone(),
        })
        .collect();
    let analysis: Vec<AnalysisCheck> = offered
        .analysis
        .iter()
        .filter(|step| step.unavailable.is_none())
        .map(|step| AnalysisCheck {
            id: step.id.clone(),
            label: format!("{} (per photo)", step.title),
            checked: form.analysis.contains(&step.id),
            before: step
                .overwrites
                .iter()
                .find(|group| checks.iter().any(|check| &check.id == *group))
                .cloned(),
        })
        .collect();
    let can_create = can_manage
        && inputs.document.state.is_some()
        && inputs.document.display_entry.is_some()
        && !inputs.busy
        && !library.pending
        && !form.name.trim().is_empty()
        && !form.group.trim().is_empty()
        && (analysis.iter().any(|step| step.checked) || checks.iter().any(|check| check.checked));
    PresetsModel {
        amount: library.interaction.amount,
        amount_enabled: enabled
            && !inputs.gesture_conflicted
            && !library.presets.as_deref().unwrap_or_default().is_empty()
            && amount_reason.is_none()
            && (inputs.gesture.is_none()
                || inputs
                    .dragging
                    .is_some_and(|(a, p)| a == action && p == "amount")
                || library.interaction.hovered.is_some()),
        amount_reason,
        amount_edit: library.interaction.editing.clone(),
        amount_invalid: library.interaction.invalid.clone(),
        amount_dragging: inputs
            .dragging
            .is_some_and(|(a, p)| a == action && p == "amount"),
        selected: selected.map(|p| p.id.as_str().to_owned()),
        action: action.to_owned(),
        empty: library.presets.as_ref().is_some_and(Vec::is_empty),
        loading: !library.ready(),
        error: library.error.clone(),
        groups,
        form: PresetFormModel {
            analysis,
            enabled: can_manage,
            open: form.open,
            name: form.name.clone(),
            group: form.group.clone(),
            checks,
            error: form.error.clone(),
            can_create,
        },
        can_import: can_manage && !library.pending,
        can_manage,
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
            let mut fields = apply_fields(declared, preset)?;
            let amount = inputs.presets.interaction.amount;
            if amount != 100.0
                && luxforge_core::settings_at_amount(inputs.modules, &preset.settings, 0.0).is_ok()
            {
                fields.insert("amount".into(), json!(amount));
            }
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

    /// The form offers exactly the core's groups for the modules this run lists, under their
    /// titles, with the core's defaults: a developer module's groups only in a developer run.
    #[test]
    fn the_form_offers_the_cores_groups_for_the_listed_modules() {
        let modules = descriptors();
        let ordinary = settings_groups(&modules, false);
        let builtin =
            luxforge_core::settings_groups(luxforge_core::ModuleRegistry::builtin().descriptors());
        assert_eq!(
            ordinary, builtin,
            "a developer module adds nothing outside a developer run"
        );
        let titles: Vec<&str> = ordinary
            .groups
            .iter()
            .map(|group| group.title.as_str())
            .collect();
        assert_eq!(
            titles,
            [
                "Basic \u{00b7} White balance",
                "Basic \u{00b7} Tone",
                "Basic \u{00b7} Colour",
                "Presence \u{00b7} Presence",
                "Tone curve \u{00b7} Tone curve",
                "Detail \u{00b7} Sharpening",
                "Detail \u{00b7} Noise reduction",
                "Colour mixer \u{00b7} HSL",
                "Colour mixer \u{00b7} Grading",
                "Vignette \u{00b7} Vignette",
                "Grain \u{00b7} Grain",
            ],
            "RAW, transforms and crop declare no field patch; Perspective's patch is not presettable"
        );
        assert_eq!(ordinary.groups[1].id, "luxforge.basic/tone");
    }

    #[test]
    fn the_checked_groups_and_steps_become_exactly_the_capture_ids() {
        let groups = settings_groups(&descriptors(), false);
        let mut form = PresetForm::default();
        // By default everything but white balance, and no analysis step.
        let ids = form.capture_ids(&groups);
        assert_eq!(ids.len(), groups.groups.len() - 1);
        assert!(!ids.iter().any(|id| id == "luxforge.basic/white-balance"));
        // Tone alone, as the smoke scenario creates it.
        for group in &groups.groups {
            form.checked
                .insert(group.id.clone(), group.id == "luxforge.basic/tone");
        }
        assert_eq!(form.capture_ids(&groups), ["luxforge.basic/tone"]);
        // Auto tone overwrites Tone, so checking it clears Tone and adds the step.
        form.analysis.insert("auto-tone".into());
        let tone = groups.group("luxforge.basic/tone").unwrap();
        assert!(form.overwritten(tone) && !form.is_checked(tone));
        assert_eq!(form.capture_ids(&groups), ["auto-tone"]);
        // White balance stays optional beside it.
        form.checked
            .insert("luxforge.basic/white-balance".into(), true);
        assert_eq!(
            form.capture_ids(&groups),
            ["luxforge.basic/white-balance", "auto-tone"]
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
                "origin": {"kind": "preset", "name": "Warm", "preset_id": preset.id.as_str()},
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
