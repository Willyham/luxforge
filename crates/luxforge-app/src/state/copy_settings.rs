//! Per-window settings clipboard and chooser. Captures and pastes use the command service.
use super::presets::{PresetForm, PresettableGroup};
use luxforge_core::{AssetId, EntryId, SourceTag};
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
    pub groups: Vec<String>,
    pub copied_at: u64,
    pub capture: Value,
}

impl Clipboard {
    pub(crate) fn parameters(&self) -> Map<String, Value> {
        serde_json::json!({"settings": self.settings, "source": self.source.name, "source-asset": self.source.asset})
            .as_object().unwrap().clone()
    }
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
    /// Late responses cannot reopen a cancelled chooser or replace a newer copy.
    pub serial: u64,
    pub previous: Option<Source>,
    pub previous_target: Option<AssetId>,
    pub shown: Option<Source>,
    pub request: Option<Value>,
    pub paste_request: Option<(String, Arc<Clipboard>, EntryId)>,
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
