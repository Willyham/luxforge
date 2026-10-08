//! Per-window settings clipboard and chooser. Captures and pastes use the command service.
use super::presets::{PresetForm, PresettableGroup};
use luxforge_core::{
    AssetId, EntryId, MAX_SOURCE_NAME, MutationOutcome, SettingsOrigin, SkippedSetting, SourceTag,
};
use serde_json::{Map, Value};
use std::{collections::BTreeMap, sync::Arc};

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Source {
    pub asset: AssetId,
    pub entry: Option<EntryId>,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Clipboard {
    pub source: Source,
    pub kind: SourceTag,
    pub settings: Map<String, Value>,
    /// The groups copied, with the fields each captured.
    pub groups: Vec<PresettableGroup>,
    pub copied_at: u64,
    pub capture: Value,
}

impl Clipboard {
    /// The copied groups' labels, in the chooser's order.
    pub(crate) fn labels(&self) -> Vec<&str> {
        self.groups
            .iter()
            .map(|group| group.label.as_str())
            .collect()
    }

    /// Where the settings came from, as `edit.apply-settings` and `batch.apply-settings` take it:
    /// the source photograph's file name, any a platform allows, and its identity.
    pub(crate) fn origin(&self) -> SettingsOrigin {
        SettingsOrigin::Paste {
            source: source_name(&self.source.name),
            source_asset: Some(self.source.asset.clone()),
        }
    }

    /// `edit.apply-settings`'s parameters: the settings and their origin.
    pub(crate) fn parameters(&self) -> Map<String, Value> {
        let mut parameters = Map::new();
        parameters.insert("settings".into(), Value::Object(self.settings.clone()));
        parameters.insert(
            "origin".into(),
            serde_json::to_value(self.origin()).unwrap_or(Value::Null),
        );
        parameters
    }

    /// The fields a copied group holds in the settings: its own fields as captured, and a
    /// variant's action whole, as capture resolves the group on a photo of that kind (Basic's White
    /// balance as the RAW development's).
    fn captured<'a>(&'a self, group: &'a PresettableGroup) -> Vec<(&'a str, &'a str)> {
        let held = |action: &'a str| {
            self.settings
                .get(action)
                .and_then(Value::as_object)
                .into_iter()
                .flat_map(|fields| fields.keys())
                .map(move |field| (action, field.as_str()))
        };
        group
            .fields
            .iter()
            .flat_map(|(action, parameters)| {
                held(action).filter(|(_, field)| parameters.iter().any(|name| name == field))
            })
            .chain(group.variants.iter().flat_map(|action| held(action)))
            .collect()
    }

    /// What a paste into one photograph did, from its answer: how many of the copied groups it
    /// applied, which it skipped and why, or that it changed nothing.
    ///
    /// A group is skipped when every field it holds is among the answer's skipped settings; a
    /// skipped setting outside any whole skipped group is still named among the reasons.
    pub(crate) fn paste_sentence(
        &self,
        outcome: MutationOutcome,
        skipped: &[SkippedSetting],
        target: &str,
    ) -> String {
        let covered = |(action, parameter): &(&str, &str)| {
            skipped.iter().any(|setting| {
                setting.action == *action
                    && setting
                        .parameter
                        .as_deref()
                        .is_none_or(|skipped| skipped == *parameter)
            })
        };
        let skipped_groups: Vec<&str> = self
            .groups
            .iter()
            .filter(|group| {
                let captured = self.captured(group);
                !captured.is_empty() && captured.iter().all(covered)
            })
            .map(|group| short_label(&group.label))
            .collect();
        let mut names: Vec<&str> = Vec::new();
        for name in &skipped_groups {
            if !names.contains(name) {
                names.push(name);
            }
        }
        let mut reasons: Vec<&str> = Vec::new();
        for setting in skipped {
            if !reasons.contains(&setting.reason.as_str()) {
                reasons.push(&setting.reason);
            }
        }
        let skips = match (names.as_slice(), reasons.is_empty()) {
            (_, true) => None,
            ([], false) => Some(format!("Some settings skipped: {}", reasons.join("; "))),
            (names, false) => Some(format!(
                "{} skipped: {}",
                names.join(", "),
                reasons.join("; ")
            )),
        };
        let total = self.groups.len();
        let applied = total - skipped_groups.len();
        let mut sentence = match outcome {
            MutationOutcome::NoOp if total > 0 && applied == 0 => {
                format!("Nothing pasted: none of the copied groups apply to {target}")
            }
            MutationOutcome::NoOp if skipped.is_empty() => {
                format!("Nothing changed: {target} already has these settings")
            }
            MutationOutcome::NoOp => {
                format!("Nothing changed: {target} already has the settings that apply")
            }
            MutationOutcome::Applied | MutationOutcome::Navigated if applied == total => format!(
                "Pasted {total} group{} from {}",
                if total == 1 { "" } else { "s" },
                self.source.name
            ),
            MutationOutcome::Applied | MutationOutcome::Navigated => format!(
                "Pasted {applied} of {total} groups from {}",
                self.source.name
            ),
        };
        if let Some(skips) = skips {
            sentence.push_str(" \u{b7} ");
            sentence.push_str(&skips);
        }
        if outcome != MutationOutcome::NoOp {
            sentence.push_str(" \u{b7} Undo \u{2318}Z");
        }
        sentence
    }
}

/// A group's name without its module: `Basic · White balance` is `White balance`.
fn short_label(label: &str) -> &str {
    label
        .rsplit_once(" \u{b7} ")
        .map_or(label, |(_, name)| name)
}

/// A file name as a paste's origin carries it: a control character, which a file name may hold on
/// some platforms but a history label may not, is shown as U+FFFD, and a name longer than any
/// platform allows keeps its first characters.
fn source_name(name: &str) -> String {
    let shown: String = name
        .chars()
        .map(|character| {
            if character.is_control() {
                '\u{fffd}'
            } else {
                character
            }
        })
        .collect();
    if shown.chars().count() <= MAX_SOURCE_NAME {
        return shown;
    }
    let mut kept: String = shown.chars().take(MAX_SOURCE_NAME - 1).collect();
    kept.push('\u{2026}');
    kept
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Group {
    pub group: PresettableGroup,
    pub custom: bool,
    pub reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Chooser {
    pub source: Source,
    pub inspected_entry: EntryId,
    pub kind: SourceTag,
    pub groups: Vec<Group>,
    pub form: PresetForm,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Targets {
    /// View revision and exact selection shown by the confirmation.
    Selection {
        revision: u64,
        ranges: Vec<(u32, u32)>,
        count: u32,
    },
    Assets(Vec<AssetId>),
}
impl Targets {
    pub(crate) fn count(&self) -> usize {
        match self {
            Self::Selection { count, .. } => *count as usize,
            Self::Assets(ids) => ids.len(),
        }
    }
    pub(crate) fn params(&self) -> Value {
        match self {
            Self::Selection { .. } => serde_json::json!({"kind": "selection"}),
            Self::Assets(ids) => serde_json::json!({"kind": "assets", "asset_ids": ids}),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Confirmation {
    pub clipboard: Arc<Clipboard>,
    pub targets: Targets,
    pub skip: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct CopySettings {
    pub clipboard: Option<Arc<Clipboard>>,
    pub cell_menu: Option<(usize, (f32, f32))>,
    pub remembered: BTreeMap<String, bool>,
    pub chooser: Option<Chooser>,
    pub confirm: Option<Confirmation>,
    pub pending: bool,
    /// A Develop paste's `batch.apply-settings` is sent and its job not yet adopted. Separate from
    /// `pending` (a capture or inspection), which has no reason to refuse an export or preset batch.
    pub batch_pending: bool,
    /// Late responses cannot reopen a cancelled chooser or replace a newer copy.
    pub serial: u64,
    pub previous: Option<Source>,
    pub previous_target: Option<AssetId>,
    pub shown: Option<Source>,
    pub request: Option<Value>,
    /// The mutation request of this desktop's paste into the open photograph, whose answer the
    /// status bar reports, and what was pasted.
    pub paste_request: Option<(String, Arc<Clipboard>)>,
}

impl CopySettings {
    pub(crate) fn form(&self) -> PresetForm {
        PresetForm {
            checked: self.remembered.clone(),
            ..PresetForm::default()
        }
    }
    pub(crate) fn observe(&mut self, source: Option<Source>) {
        if let Some(source) = source {
            if self
                .shown
                .as_ref()
                .is_some_and(|shown| shown.asset != source.asset)
            {
                self.previous = self.shown.take();
            }
            self.shown = Some(source);
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct CopyModel {
    pub state: CopySettings,
    pub copy_refusal: Option<String>,
    pub paste_refusal: Option<String>,
    pub previous_refusal: Option<String>,
    pub targets: usize,
    pub menu: Option<((f32, f32), Source, Targets)>,
    pub report: Option<super::select_catalog::CatalogSheet>,
    pub batch_report: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::presets::{capture_fields, presettable_groups};
    #[test]
    fn copy_settings_default_choice_remembers_confirmed_groups() {
        let groups = presettable_groups(
            &luxforge_core::ModuleRegistry::builtin()
                .descriptors()
                .into_iter()
                .cloned()
                .collect::<Vec<_>>(),
            false,
        );
        let mut state = CopySettings::default();
        let fields = capture_fields(&groups, &state.form());
        assert!(
            !fields["set-basic"]
                .as_array()
                .unwrap()
                .iter()
                .any(|name| name == "temperature" || name == "tint")
        );
        assert!(
            fields["set-basic"]
                .as_array()
                .unwrap()
                .iter()
                .any(|name| name == "exposure")
        );
        state.remembered = groups
            .iter()
            .map(|group| (group.label.clone(), group.label.ends_with("Tone")))
            .collect();
        let selected = capture_fields(&groups, &state.form());
        assert_eq!(selected.len(), 1);
        assert!(
            selected["set-basic"]
                .as_array()
                .unwrap()
                .iter()
                .any(|name| name == "exposure")
        );
    }
    fn group(label: &str, action: &str, fields: &[&str], variants: &[&str]) -> PresettableGroup {
        PresettableGroup {
            label: label.into(),
            fields: vec![(
                action.into(),
                fields.iter().map(|field| (*field).to_owned()).collect(),
            )],
            variants: variants.iter().map(|action| (*action).to_owned()).collect(),
            default_checked: true,
            auto_overwritten: false,
        }
    }

    /// A clipboard of Tone and White balance copied from a photo of `kind`, holding the settings a
    /// capture resolves them to there: Basic's pair on a JPEG, the RAW development on a RAW photo.
    fn clipboard(kind: SourceTag) -> Clipboard {
        let settings = match kind {
            SourceTag::Raw => serde_json::json!({
                "set-basic": {"exposure": 0.5, "contrast": 10},
                "set-raw": {"white-balance": "as-shot"},
            }),
            _ => serde_json::json!({
                "set-basic": {"exposure": 0.5, "contrast": 10, "temperature": 5, "tint": 1},
            }),
        };
        Clipboard {
            source: Source {
                asset: AssetId::new(),
                entry: None,
                name: "DSC_4471.NEF".into(),
            },
            kind,
            settings: settings.as_object().unwrap().clone(),
            groups: vec![
                group("Basic · Tone", "set-basic", &["exposure", "contrast"], &[]),
                group(
                    "Basic · White balance",
                    "set-basic",
                    &["temperature", "tint"],
                    &["set-raw"],
                ),
            ],
            copied_at: 0,
            capture: Value::Null,
        }
    }

    fn skip(action: &str, parameter: Option<&str>, reason: &str) -> SkippedSetting {
        SkippedSetting {
            action: action.into(),
            parameter: parameter.map(str::to_owned),
            reason: reason.into(),
        }
    }

    /// The sentence counts the groups the answer applied: a group is skipped when every field it
    /// captured is skipped, the whole action (a RAW development on a JPEG) or field by field
    /// (a JPEG's white balance, which a RAW photo's development supersedes).
    #[test]
    fn a_paste_sentence_reports_applied_and_skipped_groups_from_the_answer() {
        let raw = clipboard(SourceTag::Raw);
        let jpeg = clipboard(SourceTag::Jpeg);
        let development = skip("set-raw", None, "RAW does not apply to a JPEG photo");
        assert_eq!(
            raw.paste_sentence(MutationOutcome::Applied, &[], "DSC.NEF"),
            "Pasted 2 groups from DSC_4471.NEF · Undo ⌘Z"
        );
        assert_eq!(
            raw.paste_sentence(
                MutationOutcome::Applied,
                std::slice::from_ref(&development),
                "IMG.JPG"
            ),
            "Pasted 1 of 2 groups from DSC_4471.NEF · White balance skipped: RAW does not apply \
             to a JPEG photo · Undo ⌘Z"
        );
        let superseded = [
            skip(
                "set-basic",
                Some("temperature"),
                "the RAW white balance sets it",
            ),
            skip("set-basic", Some("tint"), "the RAW white balance sets it"),
        ];
        assert_eq!(
            jpeg.paste_sentence(MutationOutcome::Applied, &superseded, "DSC.NEF"),
            "Pasted 1 of 2 groups from DSC_4471.NEF · White balance skipped: the RAW white \
             balance sets it · Undo ⌘Z"
        );
        // One field of a group skipped leaves the group applied, and still says why.
        assert_eq!(
            jpeg.paste_sentence(MutationOutcome::Applied, &superseded[..1], "DSC.NEF"),
            "Pasted 2 groups from DSC_4471.NEF · Some settings skipped: the RAW white balance \
             sets it · Undo ⌘Z"
        );
        assert_eq!(
            jpeg.paste_sentence(MutationOutcome::NoOp, &[], "IMG.JPG"),
            "Nothing changed: IMG.JPG already has these settings"
        );
        assert_eq!(
            raw.paste_sentence(
                MutationOutcome::NoOp,
                std::slice::from_ref(&development),
                "IMG.JPG"
            ),
            "Nothing changed: IMG.JPG already has the settings that apply · White balance \
             skipped: RAW does not apply to a JPEG photo"
        );
        let everything = [development, skip("set-basic", None, "Basic does not apply")];
        assert_eq!(
            raw.paste_sentence(MutationOutcome::NoOp, &everything, "IMG.JPG"),
            "Nothing pasted: none of the copied groups apply to IMG.JPG · Tone, White balance \
             skipped: RAW does not apply to a JPEG photo; Basic does not apply"
        );
    }

    /// Any file name a platform allows is the paste's origin as it is; a control character is
    /// shown as U+FFFD, since a history label cannot hold one.
    #[test]
    fn a_paste_origin_carries_any_file_name() {
        let mut clipboard = clipboard(SourceTag::Raw);
        let long = format!("{}.NEF", "é".repeat(251));
        clipboard.source.name = long.clone();
        let origin = clipboard.origin();
        assert_eq!(origin.name(), long);
        assert_eq!(
            serde_json::to_value(&origin).unwrap(),
            serde_json::json!({"kind": "paste", "source": long, "source_asset": clipboard.source.asset})
        );
        clipboard.source.name = "line\nbreak.jpg".into();
        assert_eq!(clipboard.origin().name(), "line\u{fffd}break.jpg");
        assert_eq!(
            source_name(&"x".repeat(300)).chars().count(),
            MAX_SOURCE_NAME
        );
        assert_eq!(
            clipboard.parameters()["origin"]["source"],
            serde_json::json!("line\u{fffd}break.jpg")
        );
    }

    #[test]
    fn copy_settings_previous_changes_only_when_the_photograph_changes() {
        let one = Source {
            asset: AssetId::new(),
            entry: None,
            name: "one.jpg".into(),
        };
        let two = Source {
            asset: AssetId::new(),
            entry: None,
            name: "two.jpg".into(),
        };
        let mut state = CopySettings::default();
        state.observe(Some(one.clone()));
        state.observe(Some(one.clone()));
        assert!(state.previous.is_none());
        state.observe(Some(two.clone()));
        assert_eq!(state.previous, Some(one));
        state.observe(None);
        assert_eq!(state.shown, Some(two));
    }
}
