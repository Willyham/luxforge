//! The Settings sheet: which tab is open, the flags as `flags.list` last answered them, the flag
//! writes waiting and the number fields' text, and the model its tabs, General, Appearance,
//! Models when a registered module holds a model, Agents ([`super::agents`]), Experiments and, in
//! a launch eligible for usage statistics, Privacy, are drawn from. The sheet is this desktop's
//! own view state, like the gallery page; the preferences
//! and flags are the host's, read and written through `preferences.read`, `preferences.set`,
//! `flags.list` and `flags.set` as any client does
//! ([design](../../../../docs/design/settings-and-flags.md)). The preferences the General tab shows
//! are held from launch by the desktop's one preference writer ([`super::preferences`]), and the
//! themes the Appearance tab lists by the theme library ([`super::themes`]).
use super::{
    ViewState,
    agents::{Agents, AgentsModel},
    capabilities::CapabilityStore,
    models::{self, ModelsModel},
    preferences::{GeneralRow, PreferenceWriter, general_rows},
    themes::{AppearanceModel, Themes, appearance},
    update::{Switches, Updates, UpdatesGroup, group},
    usage::{PrivacyModel, Usage, privacy},
};
use luxforge_core::{
    ModuleDescriptor,
    capabilities::descriptor::{ResourceDescriptor, TaskDescriptor},
    flags::{Applies, FlagList, ListedFlag},
};
use serde_json::Value;
use std::collections::{BTreeMap, VecDeque};

/// A toggle's value as the host last answered it: what the host forced, else the person's stored
/// value or the default. Off before the first `flags.list` answers and for a flag it does not
/// list, which is the safe reading of an experiment.
pub(crate) fn toggle_on(flags: Option<&FlagList>, id: &str) -> bool {
    flags
        .and_then(|flags| flags.flag(id))
        .and_then(|flag| flag.active.as_ref().unwrap_or(&flag.value).as_bool())
        .unwrap_or(false)
}

/// Every experiment flag `task` declares is on, as the host last answered: the desktop offers the
/// task only then. Off before the first `flags.list` answers, so nothing behind a flag shows at
/// launch; the host refuses the task by name either way.
pub(crate) fn task_offered(flags: Option<&FlagList>, task: &TaskDescriptor) -> bool {
    task.flags.iter().all(|flag| toggle_on(flags, flag))
}

/// The experiment flag `resource` declares, if any, is on: the desktop lists the resource only
/// then. The host still lists, installs and removes it for any client.
pub(crate) fn resource_offered(flags: Option<&FlagList>, resource: &ResourceDescriptor) -> bool {
    resource
        .flag
        .as_deref()
        .is_none_or(|flag| toggle_on(flags, flag))
}

/// One tab of the sheet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SettingsTab {
    General,
    Appearance,
    /// The models registered modules hold, only when one does.
    Models,
    /// Connecting agents through `luxforge-mcp`, and the live clients connected now.
    Agents,
    Experiments,
    /// Usage statistics, only in a launch eligible for them.
    Privacy,
}

impl SettingsTab {
    pub(crate) const ALL: [Self; 6] = [
        Self::General,
        Self::Appearance,
        Self::Models,
        Self::Agents,
        Self::Experiments,
        Self::Privacy,
    ];

    /// The tabs the rail lists, in its order: Models only when a registered module holds a model,
    /// and Privacy only in a launch eligible for usage statistics, since otherwise neither has
    /// anything to show or change.
    pub(crate) fn shown(usage_eligible: bool, models: bool) -> impl Iterator<Item = Self> {
        Self::ALL.into_iter().filter(move |tab| match tab {
            Self::Privacy => usage_eligible,
            Self::Models => models,
            _ => true,
        })
    }

    /// The name an evidence step and a frame's state use.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::General => "general",
            Self::Appearance => "appearance",
            Self::Models => "models",
            Self::Agents => "agents",
            Self::Experiments => "experiments",
            Self::Privacy => "privacy",
        }
    }

    /// The tab an evidence step names.
    pub(crate) fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|tab| tab.name() == name)
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::General => "General",
            Self::Appearance => "Appearance",
            Self::Models => "Models",
            Self::Agents => "Agents",
            Self::Experiments => "Experiments",
            Self::Privacy => "Privacy",
        }
    }
}

/// One flag write: the flag and its new value, or `None` to remove the stored value.
pub(crate) type FlagWrite = (String, Option<Value>);

/// The sheet's own state.
#[derive(Clone, Debug, Default)]
pub(crate) struct Settings {
    /// The tab on screen, or `None` while the sheet is closed.
    pub(crate) open: Option<SettingsTab>,
    /// The flags as the last `flags.list` or `flags.set` answered them.
    pub(crate) flags: Option<FlagList>,
    /// A read of the flags and the preferences is in flight.
    pub(crate) reading: bool,
    /// Reads of the flags alone in flight that follow a change another client made, which an
    /// evidence `agent` step waits for.
    pub(crate) flag_reads: usize,
    /// Why the last read or write of the flags failed, until the next one succeeds.
    pub(crate) error: Option<String>,
    /// The flag write in flight. Writes go one at a time, in the order they were made.
    pub(crate) writing: Option<FlagWrite>,
    pub(crate) waiting: VecDeque<FlagWrite>,
    /// A number field's text while it is being typed, by flag.
    pub(crate) number_text: BTreeMap<String, String>,
    /// The window asked to close while a flag write was outstanding; it closes once the last
    /// lands.
    pub(crate) closing: bool,
    /// The Agents tab: the server beside this build, the client chosen and the connected clients.
    pub(crate) agents: Agents,
}

impl Settings {
    /// No flag write is in flight or waiting.
    pub(crate) fn idle(&self) -> bool {
        self.writing.is_none() && self.waiting.is_empty()
    }

    /// The newest write made for `flag` that has not been answered yet.
    fn outstanding(&self, flag: &str) -> Option<&Option<Value>> {
        self.waiting
            .iter()
            .rev()
            .chain(self.writing.iter())
            .find(|(id, _)| id == flag)
            .map(|(_, value)| value)
    }
}

/// What one flag row draws on its trailing side.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum FlagControl {
    Toggle(bool),
    /// The options' labels and the selected option, if the value is one of them.
    Choice {
        values: Vec<String>,
        labels: Vec<String>,
        selected: Option<usize>,
    },
    /// The field's text, the range it takes and whether the text typed is not a number in it.
    Number {
        text: String,
        range: String,
        invalid: bool,
    },
}

/// One flag as the Experiments tab draws it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct FlagRow {
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) description: String,
    pub(crate) control: FlagControl,
    /// The person chose a value, so Reset is offered.
    pub(crate) can_reset: bool,
    /// When the value takes effect, and what forced this launch's.
    pub(crate) notes: Vec<String>,
    /// Why the stored value is not used.
    pub(crate) error: Option<String>,
    /// A change to this flag has not been answered yet.
    pub(crate) saving: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SettingsModel {
    pub(crate) open: Option<SettingsTab>,
    pub(crate) rows: Vec<FlagRow>,
    pub(crate) unrecognized: Vec<String>,
    /// The flags have not been read yet.
    pub(crate) loading: bool,
    pub(crate) error: Option<String>,
    /// The General tab's rows, one per preference, or none until the preferences are read.
    pub(crate) general: Vec<GeneralRow>,
    pub(crate) preferences_error: Option<String>,
    /// The Appearance tab: one row per theme the library lists.
    pub(crate) appearance: AppearanceModel,
    /// The tabs the rail lists, in order.
    pub(crate) tabs: Vec<SettingsTab>,
    /// The Privacy tab, in a launch eligible for usage statistics.
    pub(crate) privacy: Option<PrivacyModel>,
    /// The Models tab, while it is open.
    pub(crate) models: Option<ModelsModel>,
    /// The Agents tab, while it is open.
    pub(crate) agents: Option<AgentsModel>,
    /// General's Updates group.
    pub(crate) updates: UpdatesGroup,
}

/// The modules and capability statuses the Models tab is drawn from.
#[derive(Clone, Copy)]
pub(crate) struct ModelInputs<'a> {
    pub(crate) modules: &'a [ModuleDescriptor],
    pub(crate) capabilities: &'a CapabilityStore,
}

/// The sheet for this state, the preferences the writer shows, the theme library and the models.
/// Closed, it is empty, so a closed sheet costs a message nothing.
pub(crate) fn derive(
    settings: &Settings,
    preferences: &PreferenceWriter,
    themes: &Themes,
    view_state: &ViewState,
    usage: &Usage,
    models: ModelInputs<'_>,
    updates: &Updates,
) -> SettingsModel {
    let Some(open) = settings.open else {
        return SettingsModel::default();
    };
    let rows = settings
        .flags
        .iter()
        .flat_map(|list| &list.flags)
        .map(|flag| row(settings, flag))
        .collect();
    SettingsModel {
        open: Some(open),
        rows,
        unrecognized: settings
            .flags
            .as_ref()
            .map(|list| list.unrecognized.clone())
            .unwrap_or_default(),
        loading: settings.flags.is_none() && settings.error.is_none(),
        error: settings.error.clone(),
        // A change shows at once; the answer that lands confirms it or puts the truth back.
        general: general_rows(preferences),
        preferences_error: preferences.error.clone(),
        appearance: appearance(
            themes,
            preferences,
            view_state.menu.as_ref(),
            view_state.picker_open,
        ),
        tabs: SettingsTab::shown(
            usage.eligible,
            models::model_modules(models.modules, settings.flags.as_ref())
                .next()
                .is_some(),
        )
        .collect(),
        privacy: usage.eligible.then(|| privacy(usage)),
        models: (open == SettingsTab::Models).then(|| {
            models::derive(
                models.modules,
                settings.flags.as_ref(),
                models.capabilities,
                view_state.menu.as_ref(),
            )
        }),
        agents: (open == SettingsTab::Agents)
            .then(|| super::agents::derive(&settings.agents, now_ms())),
        updates: group(updates, update_switches(preferences), now_ms()),
    }
}

/// The Updates group's two switches as the preference writer shows them: the stored answer with
/// the changes outstanding laid over it, or `None` while the preferences have not been read.
fn update_switches(preferences: &PreferenceWriter) -> Option<Switches> {
    let applied = preferences.applied()?;
    Some(Switches {
        checks: applied.update_checks,
        install_automatically: applied.update_install_automatically,
        checks_saving: preferences.saving("update_checks"),
        install_saving: preferences.saving("update_install_automatically"),
    })
}

/// Milliseconds since the Unix epoch, which the last check's time is said against.
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| {
            i64::try_from(since.as_millis()).unwrap_or(i64::MAX)
        })
}

fn row(settings: &Settings, flag: &ListedFlag) -> FlagRow {
    let outstanding = settings.outstanding(&flag.id);
    // A change shows at once; the answer that lands confirms it or puts the truth back.
    let (value, stored) = match outstanding {
        Some(Some(value)) => (value.clone(), true),
        Some(None) => (flag.default.clone(), false),
        None => (flag.value.clone(), flag.stored),
    };
    let control = match flag.kind.as_str() {
        "toggle" => FlagControl::Toggle(value.as_bool().unwrap_or(false)),
        "choice" => {
            let options = flag.options.as_deref().unwrap_or_default();
            FlagControl::Choice {
                values: options.iter().map(|option| option.value.clone()).collect(),
                labels: options.iter().map(|option| option.label.clone()).collect(),
                selected: options
                    .iter()
                    .position(|option| Some(option.value.as_str()) == value.as_str()),
            }
        }
        _ => {
            let typed = settings.number_text.get(&flag.id);
            FlagControl::Number {
                text: typed
                    .cloned()
                    .unwrap_or_else(|| value.as_f64().map(number_text).unwrap_or_default()),
                range: format!(
                    "{} to {}, step {}",
                    flag.min.map(number_text).unwrap_or_default(),
                    flag.max.map(number_text).unwrap_or_default(),
                    flag.step.map(number_text).unwrap_or_default()
                ),
                invalid: typed.is_some_and(|text| parse_number(flag, text).is_none()),
            }
        }
    };
    let mut notes = Vec::new();
    if flag.applies == Applies::Launch {
        match (&flag.active, &flag.forced_by) {
            (Some(active), Some(by)) => notes.push(format!(
                "{} for this launch: {by}",
                value_text(flag, active)
            )),
            (Some(active), None) if !same(active, &value) => notes.push("Relaunch to apply".into()),
            _ => notes.push("Applies on next launch".into()),
        }
    }
    FlagRow {
        id: flag.id.clone(),
        title: flag.title.clone(),
        description: flag.description.clone(),
        control,
        can_reset: stored,
        notes,
        error: flag.error.clone().filter(|_| outstanding.is_none()),
        saving: outstanding.is_some(),
    }
}

/// `text` as a value of the number flag `flag`: a finite number in its range on its step.
pub(crate) fn parse_number(flag: &ListedFlag, text: &str) -> Option<f64> {
    let number: f64 = text.trim().parse().ok().filter(|n: &f64| n.is_finite())?;
    let (min, max, step) = (flag.min?, flag.max?, flag.step?);
    let steps = (number - min) / step;
    ((min..=max).contains(&number) && (steps - steps.round()).abs() <= 1e-9).then_some(number)
}

/// A number as a field shows it: whole numbers without a decimal point.
fn number_text(number: f64) -> String {
    if number.fract() == 0.0 && number.abs() < 1e15 {
        format!("{}", number as i64)
    } else {
        format!("{number}")
    }
}

/// Two flag values are the same value: numbers compare as numbers, so `50` is `50.0`.
fn same(a: &Value, b: &Value) -> bool {
    match (a.as_f64(), b.as_f64()) {
        (Some(a), Some(b)) => a == b,
        _ => a == b,
    }
}

/// A value as a note names it: On or Off, an option's label, or the number.
fn value_text(flag: &ListedFlag, value: &Value) -> String {
    match value {
        Value::Bool(true) => "On".into(),
        Value::Bool(false) => "Off".into(),
        Value::String(chosen) => flag
            .options
            .iter()
            .flatten()
            .find(|option| &option.value == chosen)
            .map_or_else(|| chosen.clone(), |option| option.label.clone()),
        Value::Number(number) => number.as_f64().map(number_text).unwrap_or_default(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A listing as a developer launch forced with `--developer` answers it, with the person's
    /// choice of `third` stored and an unrecognized flag's value kept.
    fn listed() -> FlagList {
        serde_json::from_value(json!({
            "flags": [
                {"id": "developer", "title": "Developer mode", "description": "Serves the test modules.",
                 "kind": "toggle", "applies": "launch", "default": true, "value": false, "stored": true,
                 "active": true, "override": "--developer"},
                {"id": "proof.choice", "title": "Proof choice", "description": "Changes nothing.",
                 "kind": "choice", "applies": "live",
                 "options": [{"value": "first", "label": "First"}, {"value": "third", "label": "Third"}],
                 "default": "first", "value": "third", "stored": true},
                {"id": "proof.number", "title": "Proof number", "description": "Changes nothing.",
                 "kind": "number", "applies": "live", "min": 0.0, "max": 100.0, "step": 5.0,
                 "default": 50.0, "value": 50.0, "stored": false, "error": "the stored value \"x\" is not used"}
            ],
            "unrecognized": ["gone"]
        }))
        .unwrap()
    }

    fn view() -> ViewState {
        ViewState::new((1440.0, 900.0))
    }

    static NO_CAPABILITIES: std::sync::LazyLock<CapabilityStore> =
        std::sync::LazyLock::new(CapabilityStore::default);

    fn no_models() -> ModelInputs<'static> {
        ModelInputs {
            modules: &[],
            capabilities: &NO_CAPABILITIES,
        }
    }

    fn open() -> Settings {
        Settings {
            open: Some(SettingsTab::Experiments),
            flags: Some(listed()),
            ..Settings::default()
        }
    }

    #[test]
    fn a_closed_sheet_derives_nothing_and_an_unread_one_is_loading() {
        assert_eq!(
            derive(
                &Settings::default(),
                &PreferenceWriter::default(),
                &Themes::default(),
                &view(),
                &Usage::default(),
                no_models(),
                &Updates::default()
            ),
            SettingsModel::default()
        );
        let unread = derive(
            &Settings {
                open: Some(SettingsTab::Experiments),
                ..Settings::default()
            },
            &PreferenceWriter::default(),
            &Themes::default(),
            &view(),
            &Usage::default(),
            no_models(),
            &Updates::default(),
        );
        assert!(unread.loading && unread.rows.is_empty());
    }

    #[test]
    fn every_kind_draws_its_control_with_its_notes() {
        let model = derive(
            &open(),
            &PreferenceWriter::default(),
            &Themes::default(),
            &view(),
            &Usage::default(),
            no_models(),
            &Updates::default(),
        );
        assert_eq!(model.unrecognized, ["gone"]);
        let [developer, choice, number] = model.rows.as_slice() else {
            panic!("three rows")
        };
        assert_eq!(developer.control, FlagControl::Toggle(false));
        assert_eq!(developer.notes, ["On for this launch: --developer"]);
        assert!(developer.can_reset);
        assert_eq!(
            choice.control,
            FlagControl::Choice {
                values: vec!["first".into(), "third".into()],
                labels: vec!["First".into(), "Third".into()],
                selected: Some(1),
            }
        );
        assert!(choice.notes.is_empty(), "a live flag applies at once");
        assert_eq!(
            number.control,
            FlagControl::Number {
                text: "50".into(),
                range: "0 to 100, step 5".into(),
                invalid: false,
            }
        );
        assert!(!number.can_reset);
        assert_eq!(
            number.error.as_deref(),
            Some("the stored value \"x\" is not used")
        );
    }

    #[test]
    fn a_launch_flag_says_when_its_value_waits_for_a_relaunch() {
        let mut settings = open();
        let developer = &mut settings.flags.as_mut().unwrap().flags[0];
        developer.forced_by = None;
        assert_eq!(
            derive(
                &settings,
                &PreferenceWriter::default(),
                &Themes::default(),
                &view(),
                &Usage::default(),
                no_models(),
                &Updates::default()
            )
            .rows[0]
                .notes,
            ["Relaunch to apply"]
        );
        settings.flags.as_mut().unwrap().flags[0].value = json!(true);
        assert_eq!(
            derive(
                &settings,
                &PreferenceWriter::default(),
                &Themes::default(),
                &view(),
                &Usage::default(),
                no_models(),
                &Updates::default()
            )
            .rows[0]
                .notes,
            ["Applies on next launch"]
        );
    }

    #[test]
    fn an_unanswered_write_shows_at_once_and_hides_the_stale_error() {
        let mut settings = open();
        settings.writing = Some(("proof.number".into(), Some(json!(75))));
        settings.waiting.push_back(("proof.choice".into(), None));
        let model = derive(
            &settings,
            &PreferenceWriter::default(),
            &Themes::default(),
            &view(),
            &Usage::default(),
            no_models(),
            &Updates::default(),
        );
        assert!(matches!(
            &model.rows[2].control,
            FlagControl::Number { text, .. } if text == "75"
        ));
        assert!(model.rows[2].saving && model.rows[2].can_reset);
        assert_eq!(model.rows[2].error, None);
        assert!(matches!(
            &model.rows[1].control,
            FlagControl::Choice {
                selected: Some(0),
                ..
            }
        ));
        assert!(!model.rows[1].can_reset);
        assert!(!settings.idle());
    }

    #[test]
    fn typed_numbers_are_checked_against_the_range_and_step() {
        let mut settings = open();
        let flag = settings.flags.as_ref().unwrap().flags[2].clone();
        assert_eq!(parse_number(&flag, " 35 "), Some(35.0));
        for text in ["", "abc", "105", "-5", "33", "NaN", "inf"] {
            assert_eq!(parse_number(&flag, text), None, "{text}");
        }
        settings
            .number_text
            .insert("proof.number".into(), "33".into());
        assert!(matches!(
            &derive(&settings, &PreferenceWriter::default(), &Themes::default(), &view(), &Usage::default(), no_models(), &Updates::default()).rows[2].control,
            FlagControl::Number { text, invalid: true, .. } if text == "33"
        ));
    }

    #[test]
    fn settings_the_privacy_tab_is_listed_only_in_an_eligible_launch() {
        let open = Settings {
            open: Some(SettingsTab::General),
            ..Settings::default()
        };
        let ineligible = derive(
            &open,
            &PreferenceWriter::default(),
            &Themes::default(),
            &view(),
            &Usage::default(),
            no_models(),
            &Updates::default(),
        );
        assert_eq!(
            ineligible.tabs,
            [
                SettingsTab::General,
                SettingsTab::Appearance,
                SettingsTab::Agents,
                SettingsTab::Experiments
            ]
        );
        assert_eq!(ineligible.privacy, None);
        let eligible = derive(
            &open,
            &PreferenceWriter::default(),
            &Themes::default(),
            &view(),
            &Usage {
                eligible: true,
                ..Usage::default()
            },
            no_models(),
            &Updates::default(),
        );
        assert_eq!(eligible.tabs.last(), Some(&SettingsTab::Privacy));
        assert!(eligible.privacy.is_some_and(|privacy| privacy.loading));
        assert_eq!(SettingsTab::parse("privacy"), Some(SettingsTab::Privacy));
    }
}
