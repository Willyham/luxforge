//! The person's preferences as this desktop holds them, and the one writer every `preferences.set`
//! the desktop sends goes through ([design](../../../../docs/design/preferences.md#desktop-writes)).
//!
//! The desktop reads the whole `preferences.read` answer once at launch and keeps it, so anything
//! that applies a preference has it from the first frame, not only while the Settings sheet is
//! open. Every change is a [`PreferenceChange`] offered to the [`PreferenceWriter`]: one call is in
//! flight, and every change made meanwhile merges into one waiting change, the newer value of a
//! field replacing the older. What the desktop shows is the stored answer with the outstanding
//! changes laid over it, so a change shows at once; the answer that lands confirms it, and a
//! refusal drops it, which puts the stored value back.
use crate::coalesce::Coalesce;
use luxforge_core::{
    MaskOverlayColour,
    preferences::{
        BrushPreference, CanvasBackground, INTERFACE_SIZES, RawLook, WindowFrame,
        WorkspacePreference,
    },
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// Declares every preference once: the answer the desktop holds, the change it sends and the
/// field-wise merge and overlay between them, so a field cannot be added to one and missed in
/// another.
macro_rules! preferences {
    ($($(#[$doc:meta])* $field:ident: $ty:ty,)*) => {
        /// Every preference as `preferences.read` and `preferences.set` answer them, with each
        /// default filled in by the core.
        #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
        pub(crate) struct Preferences {
            $($(#[$doc])* pub(crate) $field: $ty,)*
        }

        /// The preferences one `preferences.set` changes. A field left `None` is not sent and
        /// keeps its stored value; `Some(value)` sends `value`, where a nullable preference's
        /// `Some(None)` sends `null`, which removes the stored value so it follows its default.
        #[derive(Clone, Debug, Default, PartialEq, Serialize)]
        pub(crate) struct PreferenceChange {
            $(#[serde(skip_serializing_if = "Option::is_none")]
            pub(crate) $field: Option<$ty>,)*
        }

        impl PreferenceChange {
            /// Fold a newer change into this one: each field the newer change sets replaces this
            /// one's value for it, and every other field is kept.
            pub(crate) fn merge(&mut self, newer: Self) {
                $(if newer.$field.is_some() {
                    self.$field = newer.$field;
                })*
            }

            /// Lay this change over `preferences`, as the desktop shows it before it is answered.
            pub(crate) fn apply(&self, preferences: &mut Preferences) {
                $(if let Some(value) = &self.$field {
                    preferences.$field = value.clone();
                })*
            }

            /// The names of the fields this change sets, in declaration order.
            pub(crate) fn fields(&self) -> Vec<&'static str> {
                let mut fields = Vec::new();
                $(if self.$field.is_some() {
                    fields.push(stringify!($field));
                })*
                fields
            }

            /// Whether this change sets the field named `field`.
            pub(crate) fn sets(&self, field: &str) -> bool {
                $((field == stringify!($field) && self.$field.is_some()) ||)* false
            }
        }
    };
}

preferences! {
    /// The state panel's Performance section starts open.
    performance_expanded: bool,
    /// Successive edits of one control keep one history entry.
    auto_collapse_history: bool,
    /// An import commits a new RAW photo's detected lens profile as its first-open entry.
    auto_lens_profile: bool,
    /// The look a new RAW photograph's Original starts from.
    raw_look: RawLook,
    /// The tint the mask overlay is drawn in.
    mask_overlay_colour: MaskOverlayColour,
    /// The colour around the photograph.
    canvas_background: CanvasBackground,
    /// The interface size, in percent of the system's own scale.
    interface_size: u16,
    /// The catalog the desktop opens at its next launch, or `None` for the default.
    catalog: Option<PathBuf>,
    /// The remembered panels and overlays.
    workspace: WorkspacePreference,
    /// The remembered brush, or `None` for the neutral one.
    brush: Option<BrushPreference>,
    /// The remembered window frame, or `None` for the system's placement.
    window: Option<WindowFrame>,
    /// The folder `export.plan` suggests, or `None` for the original's.
    export_folder: Option<PathBuf>,
    /// The active theme's id, Luxforge Dark's when none is stored. The Appearance tab and the
    /// palette's theme entries set it; the desktop draws the theme it names.
    theme: String,
}

/// The fields whose change `preferences.set` announces in the event log: the ones a General row
/// shows, and the theme the Appearance tab chooses. The rest are the desktop's own bookkeeping and
/// announce nothing.
pub(crate) const ANNOUNCED: [&str; 8] = [
    "auto_collapse_history",
    "auto_lens_profile",
    "raw_look",
    "mask_overlay_colour",
    "canvas_background",
    "interface_size",
    "catalog",
    "theme",
];

impl PreferenceChange {
    /// The body of the `preferences.set` that sends this change.
    pub(crate) fn params(&self) -> Value {
        serde_json::to_value(self).expect("a preference change always serializes")
    }
}

/// An owner answer as the preferences it carries.
pub(crate) fn parse(answer: Value) -> Result<Preferences, String> {
    serde_json::from_value(answer).map_err(|error| format!("unreadable preferences: {error}"))
}

/// The desktop's preferences and its one writer: the answer last read or written, the change in
/// flight and the one waiting behind it.
#[derive(Debug, Default)]
pub(crate) struct PreferenceWriter {
    /// The preferences as the last `preferences.read` or `preferences.set` answered them, or
    /// `None` while none has been read.
    stored: Option<Preferences>,
    /// The change in flight. One `preferences.set` is out at a time.
    writing: Option<PreferenceChange>,
    /// Every change made since the one in flight, merged.
    waiting: Option<PreferenceChange>,
    /// Why the last read or write failed, until the next one succeeds.
    pub(crate) error: Option<String>,
    /// The window asked to close while a write was outstanding; it closes once the last lands.
    pub(crate) closing: bool,
    /// The catalog this launch opened and why, which the Catalog row shows beside the stored
    /// location.
    pub(crate) catalog: LaunchCatalog,
    /// The one `preferences.read` in flight for another client's change, and one more wanted
    /// behind it when a further change arrived meanwhile.
    pub(crate) reading: Coalesce<()>,
}

impl PreferenceWriter {
    /// The writer as the launch read leaves it.
    pub(crate) fn new(read: Result<Preferences, String>) -> Self {
        let mut writer = Self::default();
        writer.read(read);
        writer
    }

    /// Take up a `preferences.read` answer, or keep what is held and the reason it failed.
    pub(crate) fn read(&mut self, answer: Result<Preferences, String>) {
        match answer {
            Ok(preferences) => {
                self.stored = Some(preferences);
                self.error = None;
            }
            Err(reason) => self.error = Some(reason),
        }
    }

    /// Offer a change. It shows at once and waits, merged with any change already waiting, until
    /// [`Self::start`] sends it.
    pub(crate) fn offer(&mut self, change: PreferenceChange) {
        match &mut self.waiting {
            Some(waiting) => waiting.merge(change),
            None => self.waiting = Some(change),
        }
    }

    /// The change to send now, when none is in flight; it counts as in flight until
    /// [`Self::answered`].
    pub(crate) fn start(&mut self) -> Option<PreferenceChange> {
        if self.writing.is_some() {
            return None;
        }
        let change = self.waiting.take()?;
        self.writing = Some(change.clone());
        Some(change)
    }

    /// The write in flight was answered: with the preferences it left, which confirm it, or with
    /// the reason it was refused, which drops it so the stored value shows again. Hands back the
    /// change that was in flight.
    pub(crate) fn answered(
        &mut self,
        answer: Result<Preferences, String>,
    ) -> Option<PreferenceChange> {
        self.read(answer);
        self.writing.take()
    }

    /// The preferences as stored, without the outstanding changes.
    pub(crate) fn stored(&self) -> Option<&Preferences> {
        self.stored.as_ref()
    }

    /// The preferences this desktop applies and shows: the stored answer with the change in flight
    /// and the one waiting laid over it, or `None` while none has been read.
    pub(crate) fn applied(&self) -> Option<Preferences> {
        let mut preferences = self.stored.clone()?;
        for change in self.writing.iter().chain(&self.waiting) {
            change.apply(&mut preferences);
        }
        Some(preferences)
    }

    /// The theme [`Self::applied`] holds, without cloning the rest: the newest outstanding change
    /// to it, or the stored one. Read after every message, so it allocates nothing.
    pub(crate) fn applied_theme(&self) -> Option<&str> {
        self.waiting
            .iter()
            .chain(&self.writing)
            .find_map(|change| change.theme.as_deref())
            .or_else(|| self.stored.as_ref().map(|stored| stored.theme.as_str()))
    }

    /// A change to the field named `field` has not been answered yet.
    pub(crate) fn saving(&self, field: &str) -> bool {
        self.writing
            .iter()
            .chain(&self.waiting)
            .any(|change| change.sets(field))
    }

    /// The change in flight.
    pub(crate) fn writing(&self) -> Option<&PreferenceChange> {
        self.writing.as_ref()
    }

    /// The change waiting behind it.
    pub(crate) fn waiting(&self) -> Option<&PreferenceChange> {
        self.waiting.as_ref()
    }

    /// The writes outstanding: the one in flight and the one waiting.
    pub(crate) fn outstanding(&self) -> usize {
        usize::from(self.writing.is_some()) + usize::from(self.waiting.is_some())
    }

    /// No write is in flight or waiting.
    pub(crate) fn idle(&self) -> bool {
        self.outstanding() == 0
    }
}

/// One preference the Settings sheet's General tab shows as a row of its own. The rows are drawn
/// in [`GeneralPreference::ALL`]'s order; a new row is a variant here with its field, title,
/// description, control and change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GeneralPreference {
    AutoCollapseHistory,
    AutoLensProfile,
    RawLook,
    MaskOverlayColour,
    CanvasBackground,
    InterfaceSize,
    Catalog,
}

/// What a General row's description says under Auto collapse history's title.
pub(crate) const AUTO_COLLAPSE_DESCRIPTION: &str = "Successive edits of one control keep one history \
     entry: Contrast +15, \u{2212}30 and +10 leave Contrast +10, and setting a control back to where \
     it started leaves none. Collapsed entries are kept and stay readable through the API.";

/// What the lens row says under its title.
pub(crate) const AUTO_LENS_DESCRIPTION: &str = "New RAW photos get their detected lens profile as \
     a history entry when first opened. Applies from now on; photos you have already edited keep \
     their history.";

/// What the RAW look row says under its title.
pub(crate) const RAW_LOOK_DESCRIPTION: &str = "Standard is Luxforge's look; Neutral is the bare \
     development. Applies to photos added from now on.";

/// What the mask overlay colour row says under its title.
pub(crate) const MASK_OVERLAY_COLOUR_DESCRIPTION: &str =
    "The colour the mask overlay's tint is drawn in. The Masks panel's colour control sets it too.";

/// What the canvas background row says under its title.
pub(crate) const CANVAS_BACKGROUND_DESCRIPTION: &str = "The colour around the photograph. Grey is \
     an 18% grey, for judging tone the way a print is judged.";

/// What the interface size row says under its title.
pub(crate) const INTERFACE_SIZE_DESCRIPTION: &str = "Scales the whole interface. The photograph \
     at 100% stays one pixel per display pixel.";

/// The interface size row's labels, one per size in the core's `INTERFACE_SIZES`, in its order.
const INTERFACE_SIZE_LABELS: [&str; INTERFACE_SIZES.len()] = ["100%", "110%", "125%", "150%"];

/// What the catalog row says under its title.
pub(crate) const CATALOG_DESCRIPTION: &str = "Luxforge opens the catalog in this folder, creating \
     one if there is none; the current catalog stays where it is.";

/// The file a chosen catalog folder holds, as the default catalog's folder does.
pub(crate) const CATALOG_FILE: &str = "catalog.sqlite";

/// What took precedence over the stored catalog location for this launch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CatalogOverride {
    /// `--catalog` named the catalog.
    CommandLine,
    /// An evidence run keeps its catalog inside its evidence directory.
    Evidence,
}

/// The catalog this launch opened, decided once before the owner starts
/// ([design](../../../../docs/design/preferences.md#behaviour)).
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct LaunchCatalog {
    /// The catalog file this launch opened.
    pub(crate) path: PathBuf,
    /// The catalog an ordinary launch opens with no location stored: [`CATALOG_FILE`] in the
    /// configuration directory, or `None` when there is no such directory.
    pub(crate) default: Option<PathBuf>,
    /// What took precedence over the stored location, if anything.
    pub(crate) forced: Option<CatalogOverride>,
    /// The stored catalog whose folder did not exist at launch, so this launch opened the default
    /// instead. The stored location is kept for the next launch.
    pub(crate) missing: Option<PathBuf>,
}

impl LaunchCatalog {
    /// The catalog a launch opens: `--catalog`, then an evidence run's own, then the stored
    /// location when its folder exists, then the default. A stored catalog whose folder is missing,
    /// as with an unplugged drive, opens the default and is kept as [`Self::missing`]; a folder
    /// without a catalog gets one, as the default's does. `None` when nothing names a catalog and
    /// there is no configuration directory to hold the default.
    pub(crate) fn resolve(
        command_line: Option<&Path>,
        evidence: Option<&Path>,
        default: Option<PathBuf>,
        stored: Option<PathBuf>,
    ) -> Option<Self> {
        let forced = match (command_line, evidence) {
            (Some(catalog), _) => Some((catalog.to_path_buf(), CatalogOverride::CommandLine)),
            (None, Some(evidence)) => {
                Some((evidence.join(CATALOG_FILE), CatalogOverride::Evidence))
            }
            (None, None) => None,
        };
        if let Some((path, forced)) = forced {
            return Some(Self {
                path,
                default,
                forced: Some(forced),
                missing: None,
            });
        }
        let (path, missing) = match stored {
            Some(stored) if stored.parent().is_some_and(Path::is_dir) => (stored, None),
            Some(stored) => (default.clone()?, Some(stored)),
            None => (default.clone()?, None),
        };
        Some(Self {
            path,
            default,
            forced: None,
            missing,
        })
    }

    /// What the status bar and the Catalog row say when the stored catalog's folder was missing.
    pub(crate) fn missing_note(&self) -> Option<String> {
        let missing = self.missing.as_ref()?;
        let folder = missing.parent().unwrap_or(missing);
        Some(format!(
            "Catalog folder not found: {}; using the default catalog",
            folder.display()
        ))
    }

    /// The Catalog row's notes for `stored`, the location the preferences hold: why this launch
    /// opened another catalog, and that a relaunch opens the stored one.
    pub(crate) fn notes(&self, stored: Option<&Path>) -> Vec<String> {
        let mut notes = Vec::new();
        notes.extend(self.missing_note());
        match self.forced {
            Some(CatalogOverride::CommandLine) => notes.push("This launch uses --catalog".into()),
            Some(CatalogOverride::Evidence) => {
                notes.push("This launch uses the evidence run's catalog".into())
            }
            None => {}
        }
        // The catalog an ordinary launch would open next. A launch that took its catalog from
        // elsewhere says so above, so with nothing stored there is no chosen location to relaunch
        // into; and a missing folder's own note covers the location it kept.
        let next = match stored {
            Some(stored) => Some(stored),
            None if self.forced.is_none() => self.default.as_deref(),
            None => None,
        };
        if let Some(next) = next
            .filter(|next| *next != self.path.as_path() && self.missing.as_deref() != Some(*next))
        {
            notes.push(format!("Relaunch to use {}", next.display()));
        }
        notes
    }
}

impl GeneralPreference {
    pub(crate) const ALL: [Self; 7] = [
        Self::AutoCollapseHistory,
        Self::AutoLensProfile,
        Self::RawLook,
        Self::MaskOverlayColour,
        Self::CanvasBackground,
        Self::InterfaceSize,
        Self::Catalog,
    ];

    /// The `preferences.set` field the row writes, which an evidence step and a frame's state use
    /// as its name.
    pub(crate) fn field(self) -> &'static str {
        match self {
            Self::AutoCollapseHistory => "auto_collapse_history",
            Self::AutoLensProfile => "auto_lens_profile",
            Self::RawLook => "raw_look",
            Self::MaskOverlayColour => "mask_overlay_colour",
            Self::CanvasBackground => "canvas_background",
            Self::InterfaceSize => "interface_size",
            Self::Catalog => "catalog",
        }
    }

    /// The row for a field name, if the General tab shows one.
    pub(crate) fn parse(field: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|row| row.field() == field)
    }

    pub(crate) fn title(self) -> &'static str {
        match self {
            Self::AutoCollapseHistory => "Auto collapse history",
            Self::AutoLensProfile => "Correct lens distortion on new RAW photos",
            Self::RawLook => "Starting look for new RAW photos",
            Self::MaskOverlayColour => "Mask overlay colour",
            Self::CanvasBackground => "Canvas background",
            Self::InterfaceSize => "Interface size",
            Self::Catalog => "Catalog",
        }
    }

    pub(crate) fn description(self) -> &'static str {
        match self {
            Self::AutoCollapseHistory => AUTO_COLLAPSE_DESCRIPTION,
            Self::AutoLensProfile => AUTO_LENS_DESCRIPTION,
            Self::RawLook => RAW_LOOK_DESCRIPTION,
            Self::MaskOverlayColour => MASK_OVERLAY_COLOUR_DESCRIPTION,
            Self::CanvasBackground => CANVAS_BACKGROUND_DESCRIPTION,
            Self::InterfaceSize => INTERFACE_SIZE_DESCRIPTION,
            Self::Catalog => CATALOG_DESCRIPTION,
        }
    }

    /// The control the row draws for `preferences`, with the catalog this launch opened.
    fn control(self, preferences: &Preferences, catalog: &LaunchCatalog) -> GeneralControl {
        match self {
            Self::AutoCollapseHistory => GeneralControl::Toggle(preferences.auto_collapse_history),
            Self::AutoLensProfile => GeneralControl::Toggle(preferences.auto_lens_profile),
            Self::RawLook => GeneralControl::choice(
                RawLook::ALL.map(|look| {
                    let label = match look {
                        RawLook::Standard => "Standard",
                        RawLook::Neutral => "Neutral",
                    };
                    (Value::from(look.as_str()), label)
                }),
                &Value::from(preferences.raw_look.as_str()),
            ),
            Self::MaskOverlayColour => GeneralControl::choice(
                MaskOverlayColour::ALL.map(|colour| {
                    let label = match colour {
                        MaskOverlayColour::Green => "Green",
                        MaskOverlayColour::White => "White",
                    };
                    (Value::from(colour.as_str()), label)
                }),
                &Value::from(preferences.mask_overlay_colour.as_str()),
            ),
            Self::CanvasBackground => GeneralControl::choice(
                CanvasBackground::ALL.map(|background| {
                    let label = match background {
                        CanvasBackground::Dark => "Dark",
                        CanvasBackground::Black => "Black",
                        CanvasBackground::Grey => "Grey",
                        CanvasBackground::Theme => "Theme",
                    };
                    (Value::from(background.as_str()), label)
                }),
                &Value::from(preferences.canvas_background.as_str()),
            ),
            Self::InterfaceSize => GeneralControl::choice(
                std::array::from_fn::<_, { INTERFACE_SIZES.len() }, _>(|index| {
                    (
                        Value::from(INTERFACE_SIZES[index]),
                        INTERFACE_SIZE_LABELS[index],
                    )
                }),
                &Value::from(preferences.interface_size),
            ),
            Self::Catalog => GeneralControl::Catalog {
                path: catalog.path.clone(),
                stored: preferences.catalog.clone(),
                notes: catalog.notes(preferences.catalog.as_deref()),
            },
        }
    }

    /// The change a gesture on this row's control makes, or `None` for a gesture its control does
    /// not offer.
    pub(crate) fn change(self, value: GeneralValue) -> Option<PreferenceChange> {
        let change = match (self, value) {
            (Self::AutoCollapseHistory, GeneralValue::Toggle(on)) => PreferenceChange {
                auto_collapse_history: Some(on),
                ..PreferenceChange::default()
            },
            (Self::AutoLensProfile, GeneralValue::Toggle(on)) => PreferenceChange {
                auto_lens_profile: Some(on),
                ..PreferenceChange::default()
            },
            (Self::RawLook, GeneralValue::Choice(index)) => PreferenceChange {
                raw_look: Some(*RawLook::ALL.get(index)?),
                ..PreferenceChange::default()
            },
            (Self::MaskOverlayColour, GeneralValue::Choice(index)) => PreferenceChange {
                mask_overlay_colour: Some(*MaskOverlayColour::ALL.get(index)?),
                ..PreferenceChange::default()
            },
            (Self::CanvasBackground, GeneralValue::Choice(index)) => PreferenceChange {
                canvas_background: Some(*CanvasBackground::ALL.get(index)?),
                ..PreferenceChange::default()
            },
            (Self::InterfaceSize, GeneralValue::Choice(index)) => PreferenceChange {
                interface_size: Some(*INTERFACE_SIZES.get(index)?),
                ..PreferenceChange::default()
            },
            (Self::Catalog, GeneralValue::Folder(folder)) => PreferenceChange {
                catalog: Some(Some(folder.join(CATALOG_FILE))),
                ..PreferenceChange::default()
            },
            (Self::Catalog, GeneralValue::UseDefault) => PreferenceChange {
                catalog: Some(None),
                ..PreferenceChange::default()
            },
            _ => return None,
        };
        Some(change)
    }
}

/// What a General row draws on its trailing side.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum GeneralControl {
    Toggle(bool),
    /// A segmented choice: each option's value as `preferences.set` takes it and its label, and
    /// the option the preference holds, if it is one of them.
    Choice {
        values: Vec<Value>,
        labels: Vec<&'static str>,
        selected: Option<usize>,
    },
    /// A catalog location: the catalog this launch opened, the location stored for the next
    /// launch, if any, which offers Use Default, and the notes under the row.
    Catalog {
        path: PathBuf,
        stored: Option<PathBuf>,
        notes: Vec<String>,
    },
}

impl GeneralControl {
    fn choice<const N: usize>(options: [(Value, &'static str); N], value: &Value) -> Self {
        let (values, labels): (Vec<_>, Vec<_>) = options.into_iter().unzip();
        let selected = values.iter().position(|option| option == value);
        Self::Choice {
            values,
            labels,
            selected,
        }
    }

    /// The gesture on this control that sets `value`, or `None` when the control does not offer
    /// it: a toggle takes a boolean, a choice one of its options' values, and a catalog location
    /// `null` for Use Default or an absolute `<folder>/catalog.sqlite` for that folder chosen in
    /// Choose Folder…'s dialog.
    pub(crate) fn gesture(&self, value: &Value) -> Option<GeneralValue> {
        match self {
            Self::Toggle(_) => value.as_bool().map(GeneralValue::Toggle),
            Self::Choice { values, .. } => values
                .iter()
                .position(|option| option == value)
                .map(GeneralValue::Choice),
            Self::Catalog { .. } => match value {
                Value::Null => Some(GeneralValue::UseDefault),
                Value::String(path) => {
                    let path = Path::new(path);
                    let folder = path.parent()?;
                    (path.is_absolute() && path.file_name()? == CATALOG_FILE)
                        .then(|| GeneralValue::Folder(folder.to_path_buf()))
                }
                _ => None,
            },
        }
    }

    /// Whether the control already shows the value `gesture` would set.
    pub(crate) fn shows(&self, gesture: GeneralValue) -> bool {
        match (self, gesture) {
            (Self::Toggle(on), GeneralValue::Toggle(wanted)) => *on == wanted,
            (Self::Choice { selected, .. }, GeneralValue::Choice(index)) => {
                *selected == Some(index)
            }
            (Self::Catalog { stored, .. }, GeneralValue::Folder(folder)) => {
                stored.as_deref() == Some(folder.join(CATALOG_FILE).as_path())
            }
            (Self::Catalog { stored, .. }, GeneralValue::UseDefault) => stored.is_none(),
            _ => false,
        }
    }

    /// The value the control shows, as `preferences.set` takes it.
    pub(crate) fn value(&self) -> Value {
        match self {
            Self::Toggle(on) => Value::Bool(*on),
            Self::Choice {
                values, selected, ..
            } => selected
                .and_then(|index| values.get(index).cloned())
                .unwrap_or(Value::Null),
            Self::Catalog { stored, .. } => stored
                .as_deref()
                .map_or(Value::Null, |path| Value::from(path.to_string_lossy())),
        }
    }
}

/// One gesture on a General row's control: a switch turned on or off, a segment chosen, or a
/// catalog location's button.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum GeneralValue {
    Toggle(bool),
    Choice(usize),
    /// Choose Folder…: open the native folder dialog, which answers with [`Self::Folder`].
    ChooseFolder,
    /// A folder chosen for the catalog, which stores `<folder>/catalog.sqlite`.
    Folder(PathBuf),
    /// Use Default: remove the stored location.
    UseDefault,
}

/// One General row as the sheet draws it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct GeneralRow {
    pub(crate) preference: GeneralPreference,
    pub(crate) control: GeneralControl,
    /// A change to it has not been answered yet.
    pub(crate) saving: bool,
}

/// The General tab's rows for the preferences the writer shows, or none while none has been read.
pub(crate) fn general_rows(writer: &PreferenceWriter) -> Vec<GeneralRow> {
    let Some(preferences) = writer.applied() else {
        return Vec::new();
    };
    GeneralPreference::ALL
        .into_iter()
        .map(|preference| GeneralRow {
            preference,
            control: preference.control(&preferences, &writer.catalog),
            saving: writer.saving(preference.field()),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The answer `preferences.read` gives with nothing stored.
    fn defaults() -> Preferences {
        parse(json!({
            "performance_expanded": true,
            "auto_collapse_history": true,
            "auto_lens_profile": true,
            "raw_look": "standard",
            "mask_overlay_colour": "green",
            "canvas_background": "theme",
            "interface_size": 100,
            "catalog": null,
            "workspace": {"state_panel": true, "tools_panel": true, "thirds": false,
                          "clip_shadows": false, "clip_highlights": false},
            "brush": null,
            "window": null,
            "export_folder": null,
            "theme": "luxforge.dark"
        }))
        .unwrap()
    }

    #[test]
    fn a_change_sends_only_its_fields_and_null_for_a_reset() {
        let change = PreferenceChange {
            auto_lens_profile: Some(false),
            mask_overlay_colour: Some(MaskOverlayColour::White),
            catalog: Some(None),
            ..PreferenceChange::default()
        };
        assert_eq!(
            change.params(),
            json!({"auto_lens_profile": false, "mask_overlay_colour": "white", "catalog": null})
        );
        assert_eq!(
            change.fields(),
            ["auto_lens_profile", "mask_overlay_colour", "catalog"]
        );
        assert!(change.sets("catalog") && !change.sets("auto_collapse_history"));
        assert_eq!(PreferenceChange::default().params(), json!({}));
    }

    #[test]
    fn a_newer_value_of_a_field_replaces_the_older_and_other_fields_are_kept() {
        let mut waiting = PreferenceChange {
            auto_collapse_history: Some(false),
            performance_expanded: Some(false),
            ..PreferenceChange::default()
        };
        waiting.merge(PreferenceChange {
            auto_collapse_history: Some(true),
            mask_overlay_colour: Some(MaskOverlayColour::White),
            ..PreferenceChange::default()
        });
        assert_eq!(
            waiting.params(),
            json!({"performance_expanded": false, "auto_collapse_history": true,
                   "mask_overlay_colour": "white"})
        );
    }

    /// One call in flight; every later change merges into one waiting change; what shows is the
    /// stored answer with both laid over it; the answer confirms, and a refusal drops the change.
    #[test]
    fn the_writer_keeps_one_call_in_flight_and_one_merged_change_waiting() {
        let mut writer = PreferenceWriter::new(Ok(defaults()));
        assert!(writer.idle());
        writer.offer(PreferenceChange {
            auto_collapse_history: Some(false),
            ..PreferenceChange::default()
        });
        let first = writer.start().expect("nothing was in flight");
        assert_eq!(first.params(), json!({"auto_collapse_history": false}));
        for colour in [MaskOverlayColour::White, MaskOverlayColour::Green] {
            writer.offer(PreferenceChange {
                mask_overlay_colour: Some(colour),
                auto_lens_profile: Some(colour == MaskOverlayColour::White),
                ..PreferenceChange::default()
            });
        }
        writer.offer(PreferenceChange {
            mask_overlay_colour: Some(MaskOverlayColour::White),
            ..PreferenceChange::default()
        });
        assert_eq!(writer.start(), None, "one call in flight");
        assert_eq!(writer.outstanding(), 2);
        assert_eq!(
            writer.waiting().unwrap().params(),
            json!({"auto_lens_profile": false, "mask_overlay_colour": "white"})
        );
        // Shown at once, before any answer.
        let applied = writer.applied().unwrap();
        assert!(!applied.auto_collapse_history && !applied.auto_lens_profile);
        assert_eq!(applied.mask_overlay_colour, MaskOverlayColour::White);
        assert!(writer.saving("auto_collapse_history") && writer.saving("mask_overlay_colour"));
        assert!(!writer.saving("canvas_background"));

        // The first answer confirms its change; the waiting one goes next.
        let mut stored = defaults();
        stored.auto_collapse_history = false;
        assert_eq!(writer.answered(Ok(stored)), Some(first));
        assert!(!writer.stored().unwrap().auto_collapse_history);
        let second = writer.start().unwrap();
        assert_eq!(writer.start(), None);

        // A refusal drops it: the stored value shows again, and the reason is kept.
        assert_eq!(writer.answered(Err("validation: no".into())), Some(second));
        assert!(writer.idle());
        assert_eq!(writer.error.as_deref(), Some("validation: no"));
        let applied = writer.applied().unwrap();
        assert!(applied.auto_lens_profile, "the stored value is back");
        assert_eq!(applied.mask_overlay_colour, MaskOverlayColour::Green);
        assert!(!applied.auto_collapse_history, "the confirmed change stays");
    }

    #[test]
    fn an_unread_writer_shows_no_rows_and_keeps_why() {
        let writer = PreferenceWriter::new(Err("unreadable".into()));
        assert!(writer.applied().is_none() && general_rows(&writer).is_empty());
        assert_eq!(writer.error.as_deref(), Some("unreadable"));
    }

    #[test]
    fn every_general_row_draws_its_control_and_shows_a_change_at_once() {
        let mut writer = PreferenceWriter::new(Ok(defaults()));
        let rows = general_rows(&writer);
        let fields: Vec<_> = rows.iter().map(|row| row.preference.field()).collect();
        assert_eq!(
            fields,
            [
                "auto_collapse_history",
                "auto_lens_profile",
                "raw_look",
                "mask_overlay_colour",
                "canvas_background",
                "interface_size",
                "catalog"
            ]
        );
        assert_eq!(rows[0].preference.title(), "Auto collapse history");
        assert_eq!(
            rows[1].preference.title(),
            "Correct lens distortion on new RAW photos"
        );
        assert_eq!(
            rows[2].preference.title(),
            "Starting look for new RAW photos"
        );
        assert_eq!(rows[3].preference.title(), "Mask overlay colour");
        assert_eq!(rows[0].control, GeneralControl::Toggle(true));
        assert_eq!(rows[1].control, GeneralControl::Toggle(true));
        assert_eq!(
            rows[2].control,
            GeneralControl::Choice {
                values: vec![json!("standard"), json!("neutral")],
                labels: vec!["Standard", "Neutral"],
                selected: Some(0),
            }
        );
        assert_eq!(
            rows[3].control,
            GeneralControl::Choice {
                values: vec![json!("green"), json!("white")],
                labels: vec!["Green", "White"],
                selected: Some(0),
            }
        );
        assert!(rows.iter().all(|row| !row.saving));

        for (row, value) in [
            (
                GeneralPreference::AutoLensProfile,
                GeneralValue::Toggle(false),
            ),
            (GeneralPreference::RawLook, GeneralValue::Choice(1)),
            (
                GeneralPreference::MaskOverlayColour,
                GeneralValue::Choice(1),
            ),
        ] {
            writer.offer(row.change(value).unwrap());
        }
        let rows = general_rows(&writer);
        assert_eq!(rows[1].control, GeneralControl::Toggle(false));
        assert_eq!(rows[2].control.value(), json!("neutral"));
        assert_eq!(rows[3].control.value(), json!("white"));
        assert!(!rows[0].saving && rows[1].saving && rows[2].saving && rows[3].saving);
        assert_eq!(
            GeneralPreference::RawLook
                .change(GeneralValue::Choice(1))
                .map(|change| change.params()),
            Some(json!({"raw_look": "neutral"}))
        );
        assert_eq!(
            GeneralPreference::RawLook.change(GeneralValue::Choice(2)),
            None
        );
    }

    /// The canvas background row offers its four choices by name, Theme the default, and the
    /// interface size row its four sizes as numbers, the values `preferences.set` takes; each
    /// gesture sets its field.
    #[test]
    fn the_canvas_background_and_interface_scale_rows_offer_their_choices() {
        let mut writer = PreferenceWriter::new(Ok(defaults()));
        let rows = general_rows(&writer);
        assert_eq!(rows[4].preference.title(), "Canvas background");
        assert_eq!(
            rows[4].control,
            GeneralControl::Choice {
                values: vec![json!("dark"), json!("black"), json!("grey"), json!("theme")],
                labels: vec!["Dark", "Black", "Grey", "Theme"],
                selected: Some(3),
            }
        );
        assert_eq!(rows[5].preference.title(), "Interface size");
        assert_eq!(
            rows[5].control,
            GeneralControl::Choice {
                values: vec![json!(100), json!(110), json!(125), json!(150)],
                labels: vec!["100%", "110%", "125%", "150%"],
                selected: Some(0),
            }
        );
        assert_eq!(
            rows[5].control.gesture(&json!(125)),
            Some(GeneralValue::Choice(2))
        );
        assert_eq!(rows[5].control.gesture(&json!(120)), None);
        assert_eq!(
            GeneralPreference::CanvasBackground.change(GeneralValue::Choice(2)),
            Some(PreferenceChange {
                canvas_background: Some(CanvasBackground::Grey),
                ..PreferenceChange::default()
            })
        );
        assert_eq!(
            GeneralPreference::InterfaceSize.change(GeneralValue::Choice(3)),
            Some(PreferenceChange {
                interface_size: Some(150),
                ..PreferenceChange::default()
            })
        );
        assert_eq!(
            GeneralPreference::InterfaceSize.change(GeneralValue::Choice(4)),
            None
        );
        assert_eq!(
            GeneralPreference::CanvasBackground.change(GeneralValue::Toggle(true)),
            None
        );
        for (row, value) in [
            (GeneralPreference::CanvasBackground, GeneralValue::Choice(1)),
            (GeneralPreference::InterfaceSize, GeneralValue::Choice(1)),
        ] {
            writer.offer(row.change(value).unwrap());
        }
        let rows = general_rows(&writer);
        assert_eq!(rows[4].control.value(), json!("black"));
        assert_eq!(rows[5].control.value(), json!(110));
        assert!(rows[4].saving && rows[5].saving);
        assert_eq!(
            GeneralPreference::parse("interface_size"),
            Some(GeneralPreference::InterfaceSize)
        );
    }

    #[test]
    fn a_row_takes_only_the_gestures_its_control_offers() {
        let rows = general_rows(&PreferenceWriter::new(Ok(defaults())));
        let (toggle, choice) = (&rows[0].control, &rows[3].control);
        assert_eq!(
            toggle.gesture(&json!(false)),
            Some(GeneralValue::Toggle(false))
        );
        assert_eq!(toggle.gesture(&json!("off")), None);
        assert_eq!(
            choice.gesture(&json!("white")),
            Some(GeneralValue::Choice(1))
        );
        assert_eq!(choice.gesture(&json!("purple")), None);
        assert!(
            toggle.shows(GeneralValue::Toggle(true)) && !toggle.shows(GeneralValue::Toggle(false))
        );
        assert!(choice.shows(GeneralValue::Choice(0)));
        assert_eq!(
            GeneralPreference::AutoCollapseHistory.change(GeneralValue::Choice(0)),
            None
        );
        assert_eq!(
            GeneralPreference::MaskOverlayColour.change(GeneralValue::Choice(9)),
            None
        );
        assert_eq!(
            GeneralPreference::parse("auto_lens_profile"),
            Some(GeneralPreference::AutoLensProfile)
        );
        assert_eq!(GeneralPreference::parse("performance_expanded"), None);
    }

    #[test]
    fn the_launch_catalog_is_the_command_line_evidence_the_stored_location_or_the_default() {
        let root = luxforge_testbase::paths::temp_path("launch-catalog-resolve");
        let default = root.join("config").join(CATALOG_FILE);
        let present = root.join("Photos").join(CATALOG_FILE);
        std::fs::create_dir_all(present.parent().unwrap()).unwrap();
        let missing = root.join("Unplugged").join(CATALOG_FILE);
        let resolve = |command_line: Option<&Path>, evidence: Option<&Path>, stored: &Path| {
            LaunchCatalog::resolve(
                command_line,
                evidence,
                Some(default.clone()),
                Some(stored.to_path_buf()),
            )
            .unwrap()
        };
        let named = root.join("named.sqlite");
        assert_eq!(resolve(Some(&named), Some(&root), &present).path, named);
        assert_eq!(
            resolve(None, Some(&root), &present).path,
            root.join(CATALOG_FILE)
        );
        assert_eq!(resolve(None, None, &present).path, present);
        let fallback = resolve(None, None, &missing);
        assert_eq!(
            (&fallback.path, &fallback.missing),
            (&default, &Some(missing))
        );
        assert_eq!(
            LaunchCatalog::resolve(None, None, Some(default.clone()), None)
                .unwrap()
                .path,
            default
        );
        // Nowhere to keep the default and nothing naming a catalog.
        assert_eq!(LaunchCatalog::resolve(None, None, None, None), None);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn the_catalog_row_notes_say_why_this_launch_differs_from_the_stored_location() {
        let default = PathBuf::from("/config/catalog.sqlite");
        let chosen = PathBuf::from("/Volumes/Photos/catalog.sqlite");
        let launch = |path: &Path| LaunchCatalog {
            path: path.to_path_buf(),
            default: Some(default.clone()),
            forced: None,
            missing: None,
        };
        let relaunch = |path: &Path| format!("Relaunch to use {}", path.display());
        // This launch opened what is stored: nothing to say.
        assert!(launch(&default).notes(None).is_empty());
        assert!(launch(&chosen).notes(Some(&chosen)).is_empty());
        // A location chosen, or the default chosen back, opens at the next launch.
        assert_eq!(launch(&default).notes(Some(&chosen)), [relaunch(&chosen)]);
        assert_eq!(launch(&chosen).notes(None), [relaunch(&default)]);
        // --catalog and an evidence run say so, and still name a stored location.
        let forced = |forced| LaunchCatalog {
            forced: Some(forced),
            ..launch(Path::new("/elsewhere/catalog.sqlite"))
        };
        assert_eq!(
            forced(CatalogOverride::CommandLine).notes(None),
            ["This launch uses --catalog"]
        );
        assert_eq!(
            forced(CatalogOverride::Evidence).notes(Some(&chosen)),
            [
                "This launch uses the evidence run's catalog".to_owned(),
                relaunch(&chosen)
            ]
        );
        // A missing folder says so, and the location it kept needs no relaunch note; another
        // location chosen since does.
        let fallback = LaunchCatalog {
            missing: Some(chosen.clone()),
            ..launch(&default)
        };
        let missing = "Catalog folder not found: /Volumes/Photos; using the default catalog";
        assert_eq!(fallback.missing_note().as_deref(), Some(missing));
        assert_eq!(fallback.notes(Some(&chosen)), [missing]);
        let other = PathBuf::from("/Users/someone/Pictures/catalog.sqlite");
        assert_eq!(
            fallback.notes(Some(&other)),
            [missing.to_owned(), relaunch(&other)]
        );
    }

    #[test]
    fn the_catalog_row_takes_a_chosen_folder_or_use_default() {
        let mut writer = PreferenceWriter::new(Ok(defaults()));
        writer.catalog = LaunchCatalog {
            path: PathBuf::from("/config/catalog.sqlite"),
            default: Some(PathBuf::from("/config/catalog.sqlite")),
            forced: None,
            missing: None,
        };
        let catalog = |writer: &PreferenceWriter| {
            general_rows(writer)
                .into_iter()
                .find(|row| row.preference == GeneralPreference::Catalog)
                .unwrap()
        };
        let row = catalog(&writer);
        assert_eq!(row.preference.title(), "Catalog");
        assert_eq!(row.preference.description(), CATALOG_DESCRIPTION);
        assert_eq!(
            row.control,
            GeneralControl::Catalog {
                path: PathBuf::from("/config/catalog.sqlite"),
                stored: None,
                notes: Vec::new(),
            }
        );
        assert_eq!(row.control.value(), Value::Null);

        // A folder chosen stores the catalog file in it; Use Default stores null.
        let folder = std::env::temp_dir().join("Photos");
        let file = folder.join(CATALOG_FILE);
        let text = json!(file.to_string_lossy());
        let chosen = GeneralPreference::Catalog
            .change(GeneralValue::Folder(folder.clone()))
            .unwrap();
        assert_eq!(chosen.params(), json!({"catalog": text}));
        assert_eq!(
            GeneralPreference::Catalog
                .change(GeneralValue::UseDefault)
                .unwrap()
                .params(),
            json!({"catalog": null})
        );
        // Choose Folder… opens a dialog; it changes nothing by itself.
        assert_eq!(
            GeneralPreference::Catalog.change(GeneralValue::ChooseFolder),
            None
        );
        assert_eq!(
            GeneralPreference::AutoLensProfile.change(GeneralValue::UseDefault),
            None
        );
        writer.offer(chosen);
        let row = catalog(&writer);
        assert!(row.saving);
        assert_eq!(row.control.value(), text);
        let GeneralControl::Catalog { stored, notes, .. } = &row.control else {
            panic!("a catalog control")
        };
        assert_eq!(stored.as_ref(), Some(&file));
        assert_eq!(notes, &[format!("Relaunch to use {}", file.display())]);

        // An evidence step names the stored value: a folder's catalog file, or null.
        let control = &row.control;
        let gesture = control.gesture(&text).unwrap();
        assert_eq!(gesture, GeneralValue::Folder(folder.clone()));
        assert!(control.shows(gesture));
        assert_eq!(
            control.gesture(&json!(null)),
            Some(GeneralValue::UseDefault)
        );
        assert!(!control.shows(GeneralValue::UseDefault));
        for refused in [
            json!("relative/catalog.sqlite"),
            json!(folder.join("other.sqlite").to_string_lossy()),
            json!(true),
        ] {
            assert_eq!(control.gesture(&refused), None, "{refused}");
        }
        assert!(!control.shows(GeneralValue::ChooseFolder));
    }
}
