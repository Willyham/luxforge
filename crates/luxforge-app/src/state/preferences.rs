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
    preferences::{BrushPreference, CanvasBackground, WindowFrame, WorkspacePreference},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;

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
}

/// The fields whose change `preferences.set` announces in the event log: the ones a General row
/// shows. The rest are the desktop's own bookkeeping and announce nothing.
pub(crate) const ANNOUNCED: [&str; 6] = [
    "auto_collapse_history",
    "auto_lens_profile",
    "mask_overlay_colour",
    "canvas_background",
    "interface_size",
    "catalog",
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
    MaskOverlayColour,
}

/// What a General row's description says under Auto collapse history's title.
pub(crate) const AUTO_COLLAPSE_DESCRIPTION: &str = "Successive edits of one control keep one history \
     entry: Contrast +15, \u{2212}30 and +10 leave Contrast +10, and setting a control back to where \
     it started leaves none. Collapsed entries are kept and stay readable through the API.";

/// What the lens row says under its title.
pub(crate) const AUTO_LENS_DESCRIPTION: &str = "New RAW photos get their detected lens profile as \
     a history entry. Applies to imports from now on; photos already in the catalog keep their \
     history.";

/// What the mask overlay colour row says under its title.
pub(crate) const MASK_OVERLAY_COLOUR_DESCRIPTION: &str =
    "The colour the mask overlay's tint is drawn in. The Masks panel's colour control sets it too.";

impl GeneralPreference {
    pub(crate) const ALL: [Self; 3] = [
        Self::AutoCollapseHistory,
        Self::AutoLensProfile,
        Self::MaskOverlayColour,
    ];

    /// The `preferences.set` field the row writes, which an evidence step and a frame's state use
    /// as its name.
    pub(crate) fn field(self) -> &'static str {
        match self {
            Self::AutoCollapseHistory => "auto_collapse_history",
            Self::AutoLensProfile => "auto_lens_profile",
            Self::MaskOverlayColour => "mask_overlay_colour",
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
            Self::MaskOverlayColour => "Mask overlay colour",
        }
    }

    pub(crate) fn description(self) -> &'static str {
        match self {
            Self::AutoCollapseHistory => AUTO_COLLAPSE_DESCRIPTION,
            Self::AutoLensProfile => AUTO_LENS_DESCRIPTION,
            Self::MaskOverlayColour => MASK_OVERLAY_COLOUR_DESCRIPTION,
        }
    }

    /// The control the row draws for `preferences`.
    fn control(self, preferences: &Preferences) -> GeneralControl {
        match self {
            Self::AutoCollapseHistory => GeneralControl::Toggle(preferences.auto_collapse_history),
            Self::AutoLensProfile => GeneralControl::Toggle(preferences.auto_lens_profile),
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
            (Self::MaskOverlayColour, GeneralValue::Choice(index)) => PreferenceChange {
                mask_overlay_colour: Some(*MaskOverlayColour::ALL.get(index)?),
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
    /// it: a toggle takes a boolean, a choice one of its options' values.
    pub(crate) fn gesture(&self, value: &Value) -> Option<GeneralValue> {
        match self {
            Self::Toggle(_) => value.as_bool().map(GeneralValue::Toggle),
            Self::Choice { values, .. } => values
                .iter()
                .position(|option| option == value)
                .map(GeneralValue::Choice),
        }
    }

    /// Whether the control already shows the value `gesture` would set.
    pub(crate) fn shows(&self, gesture: GeneralValue) -> bool {
        match (self, gesture) {
            (Self::Toggle(on), GeneralValue::Toggle(wanted)) => *on == wanted,
            (Self::Choice { selected, .. }, GeneralValue::Choice(index)) => {
                *selected == Some(index)
            }
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
        }
    }
}

/// One gesture on a General row's control: a switch turned on or off, or a segment chosen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GeneralValue {
    Toggle(bool),
    Choice(usize),
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
            control: preference.control(&preferences),
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
            "mask_overlay_colour": "green",
            "canvas_background": "dark",
            "interface_size": 100,
            "catalog": null,
            "workspace": {"state_panel": true, "tools_panel": true, "thirds": false,
                          "clip_shadows": false, "clip_highlights": false},
            "brush": null,
            "window": null,
            "export_folder": null
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
                "mask_overlay_colour"
            ]
        );
        assert_eq!(rows[0].preference.title(), "Auto collapse history");
        assert_eq!(
            rows[1].preference.title(),
            "Correct lens distortion on new RAW photos"
        );
        assert_eq!(rows[2].preference.title(), "Mask overlay colour");
        assert_eq!(rows[0].control, GeneralControl::Toggle(true));
        assert_eq!(rows[1].control, GeneralControl::Toggle(true));
        assert_eq!(
            rows[2].control,
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
            (
                GeneralPreference::MaskOverlayColour,
                GeneralValue::Choice(1),
            ),
        ] {
            writer.offer(row.change(value).unwrap());
        }
        let rows = general_rows(&writer);
        assert_eq!(rows[1].control, GeneralControl::Toggle(false));
        assert_eq!(rows[2].control.value(), json!("white"));
        assert!(!rows[0].saving && rows[1].saving && rows[2].saving);
    }

    #[test]
    fn a_row_takes_only_the_gestures_its_control_offers() {
        let rows = general_rows(&PreferenceWriter::new(Ok(defaults())));
        let (toggle, choice) = (&rows[0].control, &rows[2].control);
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
}
