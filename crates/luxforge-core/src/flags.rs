//! Feature flags: named, typed switches and values that gate features and experiments, stored as the
//! person's own preferences outside any catalog. A flag never changes pixels: it gates interface,
//! behaviour and execution paths whose output is byte-identical, and an experiment that changes
//! what a recipe renders is a module the recipe records. See
//! `docs/design/settings-and-flags.md`.
//!
//! Only an explicit choice is stored, so a flag nobody set follows its default. A stored value no
//! listed flag claims, or one that does not fit its flag's kind, is kept untouched and reported.
use crate::{Error, preferences::PreferenceStore};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeMap, path::PathBuf};

/// Developer mode: the desktop serves the test modules and shows the components gallery.
pub const DEVELOPER: &str = "developer";

/// The luminance and colour range mask kinds can be created: offered in the desktop's New mask and
/// Add component menus and accepted by `mask.create-<kind>` and `mask.add-<kind>`. Off, existing
/// range components still render, list and edit; only a new one is refused, naming this flag.
pub const RANGE_MASKS: &str = "masks.range-selections";

/// The Generative fill tier's experiment: whether a fill may be generated with a generative model
/// (`docs/design/ai-editing.md`). It gates whether the tier is offered, never how an accepted
/// patch renders.
pub const AI_GENERATIVE: &str = "ai.generative";

/// A generative fill shares the GPU: the generator runs its GPU work in short slices and waits
/// for each, so the interface's frames run between them (`docs/design/ai-editing.md`, "Sharing
/// the GPU"). Off, a fill holds the GPU for whole denoising steps. It changes how long a fill
/// takes and how the editor draws meanwhile, never what an accepted patch renders.
pub const AI_GENERATIVE_GPU_SHARING: &str = "ai.generative-gpu-sharing";

/// The analysis and object picking experiment (`docs/design/ai-editing.md`, "Analysis and the
/// per-photo cache"): whether the analysis tasks, `query.pick-object` and `task.select-object` are
/// offered and the desktop draws the hover overlay. It gates what is offered, never how a stored
/// selection renders.
pub const AI_ANALYSIS: &str = "ai.analysis";

/// The update check follows the `prerelease` channel as well as the default one, so release
/// candidates are offered: how they are qualified (`docs/design/auto-update.md`). Off, a
/// pre-release is never offered.
pub const PRERELEASE_CHANNEL: &str = "updates.prerelease-channel";

/// When a flag's consumer reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Applies {
    /// Each time it is needed, so a change takes effect at once.
    Live,
    /// Once, as the desktop starts, so a change takes effect at its next launch.
    Launch,
}

/// One option of a choice flag.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FlagOption {
    pub value: &'static str,
    pub label: &'static str,
}

/// What a flag's value is, and its default.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FlagKind {
    Toggle {
        default: bool,
    },
    Choice {
        options: &'static [FlagOption],
        default: &'static str,
    },
    /// A finite number in `min..=max` on a multiple of `step` from `min`.
    Number {
        min: f64,
        max: f64,
        step: f64,
        default: f64,
    },
}

/// One flag the registry declares.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FlagSpec {
    /// Stable, lowercase and dotted; the key its value is stored under.
    pub id: &'static str,
    pub title: &'static str,
    /// One sentence saying what the flag changes.
    pub description: &'static str,
    pub kind: FlagKind,
    pub applies: Applies,
    /// The toggle this flag refines, which it changes nothing without: Settings lists the flag
    /// only while that toggle is on. `flags.list` and `flags.set` take it either way.
    pub requires: Option<&'static str>,
}

impl FlagSpec {
    pub fn default_value(&self) -> Value {
        match self.kind {
            FlagKind::Toggle { default } => Value::Bool(default),
            FlagKind::Choice { default, .. } => Value::String(default.into()),
            FlagKind::Number { default, .. } => Value::from(default),
        }
    }

    /// Whether `value` fits this flag, and why not when it does not.
    pub fn check(&self, value: &Value) -> Result<(), String> {
        match self.kind {
            FlagKind::Toggle { .. } => value
                .is_boolean()
                .then_some(())
                .ok_or_else(|| "expected true or false".into()),
            FlagKind::Choice { options, .. } => match value.as_str() {
                Some(chosen) if options.iter().any(|option| option.value == chosen) => Ok(()),
                _ => Err(format!(
                    "expected one of {}",
                    options
                        .iter()
                        .map(|option| option.value)
                        .collect::<Vec<_>>()
                        .join(", ")
                )),
            },
            FlagKind::Number { min, max, step, .. } => {
                let range = || format!("expected a number from {min} to {max} in steps of {step}");
                let number = value.as_f64().filter(|n| n.is_finite()).ok_or_else(range)?;
                let steps = (number - min) / step;
                if !(min..=max).contains(&number) || (steps - steps.round()).abs() > 1e-9 {
                    return Err(range());
                }
                Ok(())
            }
        }
    }
}

/// Every product flag, in the order Settings lists them. The Generative tier's experiment stays
/// until the owner accepts the tier (`docs/design/ai-editing.md`).
const FLAGS: &[FlagSpec] = &[
    FlagSpec {
        id: DEVELOPER,
        title: "Developer mode",
        description: "Serves the test modules and shows the Developer button and its components gallery.",
        kind: FlagKind::Toggle {
            default: cfg!(debug_assertions),
        },
        applies: Applies::Launch,
        requires: None,
    },
    FlagSpec {
        id: RANGE_MASKS,
        title: "Luminance and colour range masks",
        description: "Offers the luminance and colour range kinds for a new mask or component; existing ones work either way.",
        kind: FlagKind::Toggle { default: false },
        applies: Applies::Live,
        requires: None,
    },
    FlagSpec {
        id: AI_GENERATIVE,
        title: "Generative fill",
        description: "Offers the Generative fill tier in Remove when a generative model is installed; an accepted fill renders the same either way.",
        kind: FlagKind::Toggle { default: false },
        applies: Applies::Live,
        requires: None,
    },
    FlagSpec {
        id: AI_GENERATIVE_GPU_SHARING,
        title: "Share the GPU during generative fill",
        description: "Runs a generative fill's GPU work in short slices so the editor keeps drawing while it runs; a fill may take a little longer.",
        kind: FlagKind::Toggle { default: false },
        applies: Applies::Live,
        requires: Some(AI_GENERATIVE),
    },
    FlagSpec {
        id: AI_ANALYSIS,
        title: "AI object picking",
        description: "Analyses the photo with the installed segmenter so the pointer can pick objects in Remove, and offers the analysis and object selection tasks; nothing renders differently either way.",
        kind: FlagKind::Toggle { default: false },
        applies: Applies::Live,
        requires: None,
    },
    FlagSpec {
        id: PRERELEASE_CHANNEL,
        title: "Pre-release updates",
        description: "Offers release candidates from the pre-release update channel as well as releases.",
        kind: FlagKind::Toggle { default: false },
        applies: Applies::Live,
        requires: None,
    },
];

/// Test fixtures, listed only by a host that serves the test modules, as the proof modules are.
/// They change nothing, so the choice and number controls have a real flag to render.
const PROOF_FLAGS: &[FlagSpec] = &[
    FlagSpec {
        id: "proof.choice",
        title: "Proof choice",
        description: "A test fixture for the choice control; changes nothing.",
        kind: FlagKind::Choice {
            options: &[
                FlagOption {
                    value: "first",
                    label: "First",
                },
                FlagOption {
                    value: "second",
                    label: "Second",
                },
                FlagOption {
                    value: "third",
                    label: "Third",
                },
            ],
            default: "first",
        },
        applies: Applies::Live,
        requires: None,
    },
    FlagSpec {
        id: "proof.number",
        title: "Proof number",
        description: "A test fixture for the number control; changes nothing.",
        kind: FlagKind::Number {
            min: 0.0,
            max: 100.0,
            step: 5.0,
            default: 50.0,
        },
        applies: Applies::Live,
        requires: None,
    },
];

/// The flags a host lists: every product flag, and the proof flags when it serves the test
/// modules.
pub(crate) fn listed(developer: bool) -> impl Iterator<Item = &'static FlagSpec> {
    FLAGS
        .iter()
        .chain(PROOF_FLAGS.iter().filter(move |_| developer))
}

/// The listed flag named `id`.
pub(crate) fn find(id: &str, developer: bool) -> Option<&'static FlagSpec> {
    listed(developer).find(|flag| flag.id == id)
}

/// One option of a listed choice flag.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListedOption {
    pub value: String,
    pub label: String,
}

/// One flag as `flags.list` answers it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListedFlag {
    pub id: String,
    pub title: String,
    pub description: String,
    /// `toggle`, `choice` or `number`.
    pub kind: String,
    pub applies: Applies,
    /// The toggle this flag refines: Settings lists the flag only while that toggle is on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requires: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<ListedOption>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<f64>,
    pub default: Value,
    /// What a live flag reads now and a launch flag's next launch reads.
    pub value: Value,
    /// The person chose a value, which `flags.set` with `null` removes.
    pub stored: bool,
    /// The value this launch resolved, for a launch flag on a host the desktop started, or the
    /// value a host forced for a live flag.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<Value>,
    /// What forced the active value for this launch, such as `--developer`.
    #[serde(default, rename = "override", skip_serializing_if = "Option::is_none")]
    pub forced_by: Option<String>,
    /// Why the stored value was not used: it does not fit the flag, which follows its default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// `flags.list`'s answer, and `flags.set`'s.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FlagList {
    pub flags: Vec<ListedFlag>,
    /// The identities of stored values no listed flag claims. They are kept.
    pub unrecognized: Vec<String>,
}

impl FlagList {
    pub fn flag(&self, id: &str) -> Option<&ListedFlag> {
        self.flags.iter().find(|flag| flag.id == id)
    }
}

/// A flag's stored value when it fits, or why it does not.
fn effective(flag: &FlagSpec, stored: Option<&Value>) -> (Value, Option<String>) {
    match stored {
        None => (flag.default_value(), None),
        Some(value) => match flag.check(value) {
            Ok(()) => (value.clone(), None),
            Err(reason) => (
                flag.default_value(),
                Some(format!("the stored value {value} is not used: {reason}")),
            ),
        },
    }
}

/// What a launch resolved for each launch flag, and each live flag it forced: its value and what
/// forced it, if anything. A host the desktop started reports these as `active`; any other host has
/// none, unless it forces a flag itself, as a test host forces an experiment on.
#[derive(Clone, Debug, Default)]
pub struct LaunchFlags {
    resolved: BTreeMap<&'static str, (Value, Option<&'static str>)>,
    /// Why the stored values could not be read, when they could not: every flag took its default.
    problem: Option<Error>,
}

impl LaunchFlags {
    /// Read the stored launch flags once from the preferences under `preferences_dir`, then apply
    /// `overrides`, each `(flag, value, what forced it)`. An override of a live flag fixes it for
    /// the host's life, whatever is stored. A file that cannot be read leaves every flag at its
    /// default and is kept as [`Self::problem`]; nothing is written.
    pub fn resolve(
        preferences_dir: Option<PathBuf>,
        overrides: &[(&'static str, Value, &'static str)],
    ) -> Self {
        let (stored, problem) = match PreferenceStore::new(preferences_dir).read() {
            Ok(preferences) => (preferences.flags, None),
            Err(error) => (BTreeMap::new(), Some(error)),
        };
        let mut resolved: BTreeMap<_, _> = FLAGS
            .iter()
            .filter(|flag| flag.applies == Applies::Launch)
            .map(|flag| (flag.id, (effective(flag, stored.get(flag.id)).0, None)))
            .collect();
        for (id, value, by) in overrides {
            if let Some(flag) = FLAGS.iter().find(|flag| flag.id == *id) {
                resolved.insert(flag.id, (value.clone(), Some(*by)));
            }
        }
        Self { resolved, problem }
    }

    /// The value this host forced for a live flag, if it forced one.
    fn forced(&self, id: &str) -> Option<&Value> {
        self.resolved
            .get(id)
            .and_then(|(value, by)| by.is_some().then_some(value))
    }

    /// A launch toggle's resolved value; `false` for anything else.
    pub fn toggle(&self, id: &str) -> bool {
        self.resolved
            .get(id)
            .and_then(|(value, _)| value.as_bool())
            .unwrap_or(false)
    }

    /// Why the stored values could not be read at launch.
    pub fn problem(&self) -> Option<&Error> {
        self.problem.as_ref()
    }
}

/// A live toggle's value now: what the host forced, or else its stored value when that fits, or
/// else its default, which is also what a store that cannot be read gives.
pub(crate) fn live_toggle(store: &PreferenceStore, launch: &LaunchFlags, id: &str) -> bool {
    let stored = store.read().ok().map(|preferences| preferences.flags);
    live_toggle_in(stored.as_ref(), launch, id)
}

/// [`live_toggle`] from flags already read: `None` for a store that could not be read.
pub(crate) fn live_toggle_in(
    stored: Option<&BTreeMap<String, Value>>,
    launch: &LaunchFlags,
    id: &str,
) -> bool {
    let Some(flag) = FLAGS.iter().find(|flag| flag.id == id) else {
        return false;
    };
    let value = match launch.forced(id) {
        Some(value) => value.clone(),
        None => effective(flag, stored.and_then(|flags| flags.get(id))).0,
    };
    value.as_bool().unwrap_or(false)
}

/// The flags a host lists, read from its store now, with what its launch resolved.
pub(crate) fn list(
    store: &PreferenceStore,
    launch: &LaunchFlags,
    developer: bool,
) -> Result<FlagList, Error> {
    let stored = store.read()?.flags;
    let flags = listed(developer)
        .map(|flag| {
            let (value, error) = effective(flag, stored.get(flag.id));
            let launched = launch.resolved.get(flag.id);
            let (kind, options, range) = match flag.kind {
                FlagKind::Toggle { .. } => ("toggle", None, None),
                FlagKind::Choice { options, .. } => (
                    "choice",
                    Some(
                        options
                            .iter()
                            .map(|option| ListedOption {
                                value: option.value.into(),
                                label: option.label.into(),
                            })
                            .collect(),
                    ),
                    None,
                ),
                FlagKind::Number { min, max, step, .. } => ("number", None, Some((min, max, step))),
            };
            ListedFlag {
                id: flag.id.into(),
                title: flag.title.into(),
                description: flag.description.into(),
                kind: kind.into(),
                applies: flag.applies,
                requires: flag.requires.map(str::to_owned),
                options,
                min: range.map(|(min, _, _)| min),
                max: range.map(|(_, max, _)| max),
                step: range.map(|(_, _, step)| step),
                default: flag.default_value(),
                value,
                stored: stored.contains_key(flag.id),
                active: launched.map(|(value, _)| value.clone()),
                forced_by: launched.and_then(|(_, by)| by.map(str::to_owned)),
                error,
            }
        })
        .collect();
    let unrecognized = stored
        .keys()
        .filter(|id| find(id, developer).is_none())
        .cloned()
        .collect();
    Ok(FlagList {
        flags,
        unrecognized,
    })
}

/// Store `value` for the listed flag `id`, or remove its stored value for `None`. Returns whether
/// the stored value changed.
#[cfg(test)]
pub(crate) fn set(
    store: &PreferenceStore,
    id: &str,
    value: Option<Value>,
    developer: bool,
) -> Result<bool, Error> {
    set_published(store, id, value, developer)?.finish()
}

pub(crate) fn set_published(
    store: &PreferenceStore,
    id: &str,
    value: Option<Value>,
    developer: bool,
) -> Result<crate::atomic_file::Publication<bool>, Error> {
    let flag = find(id, developer).ok_or_else(|| Error::validation(format!("no flag {id}")))?;
    if let Some(value) = &value {
        flag.check(value)
            .map_err(|reason| Error::validation(format!("flag {id}: {reason}")))?;
    }
    store.set_flag_published(flag.id, value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ErrorKind;
    use luxforge_testbase::paths::temp_path;
    use serde_json::json;

    fn store(name: &str) -> (PathBuf, PreferenceStore) {
        let root = temp_path(name);
        (root.clone(), PreferenceStore::new(Some(root)))
    }

    #[test]
    fn flags_follow_their_default_until_set_and_again_once_reset() {
        let (root, store) = store("flags-reset");
        let launch = LaunchFlags::default();
        let listed = list(&store, &launch, true).unwrap();
        let number = listed.flag("proof.number").unwrap();
        assert_eq!((number.value.clone(), number.stored), (json!(50.0), false));
        assert!(!root.exists(), "listing creates nothing");

        assert!(set(&store, "proof.number", Some(json!(75)), true).unwrap());
        assert!(!set(&store, "proof.number", Some(json!(75)), true).unwrap());
        let number = list(&store, &launch, true).unwrap();
        let number = number.flag("proof.number").unwrap();
        assert_eq!((number.value.clone(), number.stored), (json!(75), true));

        assert!(set(&store, "proof.number", None, true).unwrap());
        let number = list(&store, &launch, true).unwrap();
        let number = number.flag("proof.number").unwrap();
        assert_eq!((number.value.clone(), number.stored), (json!(50.0), false));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn every_bad_value_is_refused_by_name_and_writes_nothing() {
        let (root, store) = store("flags-refused");
        for (id, value, reason) in [
            (
                "developer",
                json!("yes"),
                "flag developer: expected true or false",
            ),
            (
                "proof.choice",
                json!("fourth"),
                "flag proof.choice: expected one of first, second, third",
            ),
            (
                "proof.number",
                json!(101),
                "flag proof.number: expected a number from 0 to 100 in steps of 5",
            ),
            (
                "proof.number",
                json!(12),
                "flag proof.number: expected a number from 0 to 100 in steps of 5",
            ),
            ("missing", json!(true), "no flag missing"),
        ] {
            let error = set(&store, id, Some(value), true).unwrap_err();
            assert_eq!(
                (error.kind, error.detail.as_str()),
                (ErrorKind::Validation, reason)
            );
        }
        let error = set(&store, "proof.number", Some(json!(10)), false).unwrap_err();
        assert_eq!(
            error.detail, "no flag proof.number",
            "proof flags need developer mode"
        );
        assert!(!root.exists());
    }

    #[test]
    fn unrecognized_and_mismatched_stored_values_are_reported_and_kept() {
        let (root, store) = store("flags-kept");
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("preferences.json");
        std::fs::write(
            &path,
            br#"{"format":1,"performance_expanded":true,"flags":{"developer":"on","gone":3,"proof.choice":"second"}}"#,
        )
        .unwrap();
        let listed = list(&store, &LaunchFlags::default(), false).unwrap();
        let developer = listed.flag(DEVELOPER).unwrap();
        assert_eq!(developer.value, json!(cfg!(debug_assertions)));
        assert!(developer.stored);
        assert_eq!(
            developer.error.as_deref(),
            Some("the stored value \"on\" is not used: expected true or false")
        );
        assert_eq!(listed.unrecognized, ["gone", "proof.choice"]);

        set(&store, DEVELOPER, Some(json!(true)), false).unwrap();
        let written: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            written["flags"],
            json!({"developer": true, "gone": 3, "proof.choice": "second"})
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_launch_resolves_the_stored_value_then_the_command_line() {
        let (root, store) = store("flags-launch");
        let resolve = |forced: bool| {
            let overrides = if forced {
                vec![(DEVELOPER, json!(true), "--developer")]
            } else {
                vec![]
            };
            LaunchFlags::resolve(Some(root.clone()), &overrides)
        };
        assert_eq!(resolve(false).toggle(DEVELOPER), cfg!(debug_assertions));
        set(&store, DEVELOPER, Some(json!(false)), false).unwrap();
        assert!(!resolve(false).toggle(DEVELOPER));
        let forced = resolve(true);
        assert!(forced.toggle(DEVELOPER));
        let listed = list(&store, &forced, false).unwrap();
        let developer = listed.flag(DEVELOPER).unwrap();
        assert_eq!(
            (
                &developer.value,
                &developer.active,
                developer.forced_by.as_deref()
            ),
            (&json!(false), &Some(json!(true)), Some("--developer"))
        );
        assert!(
            list(&store, &LaunchFlags::default(), false).unwrap().flags[0]
                .active
                .is_none(),
            "a host the desktop did not start reports no active value"
        );

        std::fs::write(root.join("preferences.json"), b"{\"format\":9}").unwrap();
        let unreadable = resolve(false);
        assert_eq!(unreadable.toggle(DEVELOPER), cfg!(debug_assertions));
        assert_eq!(unreadable.problem().unwrap().kind, ErrorKind::Incompatible);
        assert_eq!(
            std::fs::read(root.join("preferences.json")).unwrap(),
            b"{\"format\":9}"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn proof_flags_are_listed_only_to_a_developer_host() {
        let (_, store) = store("flags-proof");
        let ids = |developer| {
            list(&store, &LaunchFlags::default(), developer)
                .unwrap()
                .flags
                .into_iter()
                .map(|flag| flag.id)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            ids(false),
            [
                "developer",
                RANGE_MASKS,
                AI_GENERATIVE,
                AI_GENERATIVE_GPU_SHARING,
                AI_ANALYSIS,
                PRERELEASE_CHANNEL
            ]
        );
        assert_eq!(
            ids(true),
            [
                "developer",
                RANGE_MASKS,
                AI_GENERATIVE,
                AI_GENERATIVE_GPU_SHARING,
                AI_ANALYSIS,
                PRERELEASE_CHANNEL,
                "proof.choice",
                "proof.number"
            ]
        );
    }

    #[test]
    fn a_refining_flag_requires_a_listed_toggle_and_its_listing_names_it() {
        for flag in listed(true) {
            if let Some(parent) = flag.requires {
                let parent = find(parent, false).expect("a listed product flag");
                assert!(
                    matches!(parent.kind, FlagKind::Toggle { .. }),
                    "{}",
                    flag.id
                );
                assert!(parent.requires.is_none(), "{} nests", flag.id);
            }
        }
        let (_, store) = store("flags-requires");
        let listed = list(&store, &LaunchFlags::default(), false).unwrap();
        let requires = |id: &str| listed.flag(id).unwrap().requires.clone();
        assert_eq!(
            requires(AI_GENERATIVE_GPU_SHARING).as_deref(),
            Some(AI_GENERATIVE)
        );
        assert_eq!(requires(AI_GENERATIVE), None);
    }

    #[test]
    fn a_listing_reads_back_as_the_type_it_was_written_from() {
        let (_, store) = store("flags-round-trip");
        let listed = list(&store, &LaunchFlags::default(), true).unwrap();
        let text = serde_json::to_value(&listed).unwrap();
        assert_eq!(
            text["flags"][6]["options"][1],
            json!({"value":"second","label":"Second"})
        );
        assert_eq!(serde_json::from_value::<FlagList>(text).unwrap(), listed);
    }

    #[test]
    fn flags_set_is_announced_and_listed_beside_flags_list_and_touches_no_asset() {
        use crate::{ApiRequest, HostConfig, ModuleRegistry, OwnerHandle};
        let root = temp_path("flags-owner");
        let (owner, join) = OwnerHandle::start_with_host(
            &root.join("catalog.sqlite"),
            std::sync::Arc::new(ModuleRegistry::developer()),
            HostConfig {
                preferences_dir: Some(root.join("config")),
                ..HostConfig::unconfigured()
            },
        )
        .unwrap();
        let client = owner.register();
        let call = |id: &str, method: &str, params: Value| {
            owner
                .call(
                    client,
                    ApiRequest {
                        id: id.into(),
                        method: method.into(),
                        params,
                        token: None,
                    },
                )
                .unwrap()
        };
        let schemas = call("schema", "schema.list", json!({})).result.unwrap();
        assert!(schemas["methods"]["flags.list"].is_object());
        assert!(schemas["methods"]["flags.set"].is_object());

        let listed = call("list", "flags.list", json!({})).result.unwrap();
        assert_eq!(
            listed["flags"][6]["id"], "proof.choice",
            "a developer host lists proofs"
        );
        let set = call(
            "set",
            "flags.set",
            json!({"flag": "proof.choice", "value": "third"}),
        );
        assert_eq!(set.result.unwrap()["flags"][6]["value"], "third");
        let refused = call(
            "bad",
            "flags.set",
            json!({"flag": "proof.choice", "value": 3}),
        );
        assert_eq!(
            refused.error.unwrap().message,
            "flag proof.choice: expected one of first, second, third"
        );
        // Unchanged, so not announced again.
        call(
            "again",
            "flags.set",
            json!({"flag": "proof.choice", "value": "third"}),
        );
        call(
            "reset",
            "flags.set",
            json!({"flag": "proof.choice", "value": null}),
        );

        let events = call("events", "events.since", json!({"after": 0}))
            .result
            .unwrap();
        let events: Vec<_> = events["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|event| {
                (
                    event["method"].as_str().unwrap().to_owned(),
                    event["request_id"].as_str().unwrap().to_owned(),
                    event.get("asset_id").filter(|id| !id.is_null()).is_some(),
                )
            })
            .collect();
        assert_eq!(
            events,
            [
                ("flags.set".to_owned(), "set".to_owned(), false),
                ("flags.set".to_owned(), "reset".to_owned(), false),
            ]
        );
        owner.stop();
        join.join().unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }
}
