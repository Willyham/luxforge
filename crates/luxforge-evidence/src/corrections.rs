//! Evidence steps on the Corrections workspace (`docs/design/corrections-workspace.md`): each a
//! press on the photograph, a handle drag or a band control, sent through the message the canvas or
//! the band sends, and captured once its draft has ended, its request has answered and the list and
//! pins of the entry on screen have been read again.
use super::{content_point, text};
use serde::{Deserialize, Serialize};

/// The modifiers a scripted press holds, written as the keys they are: `["alt"]`, `["command"]`,
/// `["shift"]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PressModifier {
    Alt,
    Command,
    Shift,
}

/// The selected correction's handle a drag holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CorrectionHandle {
    Pin,
    Source,
    Size,
}

/// One Corrections gesture. Positions are normalized content coordinates, as the canvas publishes
/// them after placing the pointer through the core's map; a correction is named by its row's place
/// in the stack, top first, as the band lists it.
///
/// `{"press": {"at": [0.4, 0.5]}}` clicks the photograph away from every pin, which adds a spot;
/// `"to"` drags through positions before the release, which paints a stroke (or, with `command`,
/// pulls out a spot's source); `"modifiers": ["alt"]` sets the source; `"release": false` leaves the
/// button down so a drafted frame is captured. `{"release": [x, y]}` lets a held press go.
/// `{"pin": 0}` presses the top row's pin, which selects it. `{"drag": {"handle": "source", "to":
/// [[x, y], …]}}` drags the selected correction's handle through positions. `{"row": 1}` selects a
/// row, `{"row": null}` clears the selection. `{"row_field": {"row": 0, "name": "opacity", "fraction":
/// 0.8}}` drags a selected row's slider there and lets go. `{"row_kind": {"row": 0, "kind": "clone"}}`
/// chooses a row's kind. `{"reorder": {"row": 2, "to": 0}}` drags a row to a list position.
/// `{"menu": 0}` opens a row's menu. `{"kind": "clone"}`, `{"field": {"name": "radius", "fraction":
/// 0.4}}` and `"source_auto"` set the New correction group. `"new_source"` and `"delete"` press the
/// selected row's buttons. `"reset"` presses the band's reset.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum CorrectionStep {
    Press(PressStep),
    Release([f64; 2]),
    Pin(usize),
    Drag(DragStep),
    Row(#[serde(deserialize_with = "Option::deserialize")] Option<usize>),
    RowField(RowFieldStep),
    RowKind { row: usize, kind: String },
    Reorder { row: usize, to: usize },
    Menu(usize),
    Kind(String),
    Field { name: String, fraction: f64 },
    SourceAuto,
    NewSource,
    Delete,
    Reset,
}

/// A press on the photograph that catches no pin or handle.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PressStep {
    pub at: [f64; 2],
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub to: Vec<[f64; 2]>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub modifiers: Vec<PressModifier>,
    #[serde(default = "super::yes", skip_serializing_if = "super::is_true")]
    pub release: bool,
}

/// A drag of the selected correction's handle.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DragStep {
    pub handle: CorrectionHandle,
    pub to: Vec<[f64; 2]>,
}

/// A selected row's slider dragged to a rail position and let go.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RowFieldStep {
    pub row: usize,
    pub name: String,
    pub fraction: f64,
}

fn fraction(value: f64, field: &str) -> Result<(), String> {
    if value.is_finite() && (0.0..=1.0).contains(&value) {
        Ok(())
    } else {
        Err(format!("{field} takes a fraction from 0 to 1"))
    }
}

impl CorrectionStep {
    pub(crate) fn validate(&self) -> Result<(), String> {
        match self {
            Self::Press(press) => {
                content_point(press.at, "correction press at")?;
                press
                    .to
                    .iter()
                    .try_for_each(|point| content_point(*point, "correction press to"))
            }
            Self::Release(at) => content_point(*at, "correction release"),
            Self::Drag(drag) => {
                if drag.to.is_empty() {
                    return Err("correction drag needs at least one position".into());
                }
                drag.to
                    .iter()
                    .try_for_each(|point| content_point(*point, "correction drag to"))
            }
            Self::RowField(step) => {
                text(&step.name, "correction row_field name")?;
                fraction(step.fraction, "correction row_field fraction")
            }
            Self::RowKind { kind, .. } | Self::Kind(kind) => text(kind, "correction kind"),
            Self::Field { name, fraction: value } => {
                text(name, "correction field name")?;
                fraction(*value, "correction field fraction")
            }
            Self::Pin(_)
            | Self::Row(_)
            | Self::Reorder { .. }
            | Self::Menu(_)
            | Self::SourceAuto
            | Self::NewSource
            | Self::Delete
            | Self::Reset => Ok(()),
        }
    }
}
