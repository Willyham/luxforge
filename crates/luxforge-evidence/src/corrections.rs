//! Evidence steps on the Corrections workspace (`docs/design/corrections-workspace.md`): each a
//! press on the photograph, a handle drag or a band control, sent through the message the canvas or
//! the band sends, and captured once its draft has ended, its request has answered and the list and
//! pins of the entry on screen have been read again.
use super::{content_point, positive, text};
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
/// `"interval_ms": 24` on a press or a drag sends its positions one per 24 ms in real time rather
/// than in one update, for a latency measurement. `{"pin": 0}` presses the top row's pin, which
/// selects it. `{"drag": {"handle": "source", "to": [[x, y], …]}}` drags the selected correction's
/// handle through positions. `{"row": 1}` selects a
/// row, `{"row": null}` clears the selection. `{"row_field": {"row": 0, "name": "opacity", "fraction":
/// 0.8}}` drags a selected row's slider there and lets go. `{"row_kind": {"row": 0, "kind": "clone"}}`
/// chooses a row's kind. `{"reorder": {"row": 2, "to": 0}}` drags a row to a list position.
/// `{"menu": 0}` opens a row's menu. `{"kind": "clone"}`, `{"field": {"name": "radius", "fraction":
/// 0.4}}` and `"source_auto"` set the New correction group. `"new_source"` and `"delete"` press the
/// selected row's buttons. `"reset"` presses the band's reset. `{"kind": "remove"}` chooses Remove,
/// whose controls `{"remove": …}` presses ([`RemoveStep`]); a press then paints its selection.
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
    Remove(RemoveStep),
}

/// What a Generate or Regenerate step waits for before its frame is captured.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoveUntil {
    /// The generation reports a candidate's step, or has ended: a frame of the ring and the
    /// status bar's progress.
    Progress,
    /// The generation has ended and every candidate in the strip has its preview.
    Done,
}

/// One control of the Remove band (`docs/design/ai-editing.md`, "Desktop surface").
///
/// `{"generate": "done"}` and `{"regenerate": "done"}` press Generate (Replace, with a prompt) and
/// Regenerate and wait for the strip; `"progress"` captures the generation while it runs.
/// `"cancel"` presses Cancel. `{"candidate": 1}` chooses the strip's second candidate. `{"prompt":
/// "a red bicycle"}` types into the prompt field, replacing what it held. `{"tier": "generative"}`
/// chooses a Fill tier. `"setup"` presses the setup's button: the Replace add-on's when a prompt
/// shows it, or else the main line's when AI editing is not set up. `"accept"` presses Accept.
/// `{"hover": [0.25, 0.25]}` moves the pointer over the photograph at that normalized content
/// position, as the canvas reports a move while hover picking is on, and waits until the pick it
/// asks (and any analysis it needs first) has answered; `{"hover": null}` moves it off the
/// photograph.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum RemoveStep {
    Generate(RemoveUntil),
    Regenerate(RemoveUntil),
    Cancel,
    Candidate(usize),
    Prompt(String),
    Tier(String),
    Setup,
    Accept,
    Hover(Option<[f64; 2]>),
}

/// The longest prompt a step may type. The field holds what the fill task declares and cuts a
/// longer one there, as typing would; past this it is a script's mistake.
const MAX_PROMPT: usize = 1024;

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
    /// Send the press, each later position and the release one per this many milliseconds, in
    /// real time, instead of in one update, which is what a **latency** measurement needs: each
    /// position is then its own input with its own `draft.set`, as under a hand.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interval_ms: Option<u64>,
}

/// A drag of the selected correction's handle.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DragStep {
    pub handle: CorrectionHandle,
    pub to: Vec<[f64; 2]>,
    /// Send the handle's press and each position one per this many milliseconds, in real time,
    /// as [`PressStep::interval_ms`] does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interval_ms: Option<u64>,
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
                positive(press.interval_ms, "correction press interval_ms")?;
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
                positive(drag.interval_ms, "correction drag interval_ms")?;
                drag.to
                    .iter()
                    .try_for_each(|point| content_point(*point, "correction drag to"))
            }
            Self::RowField(step) => {
                text(&step.name, "correction row_field name")?;
                fraction(step.fraction, "correction row_field fraction")
            }
            Self::RowKind { kind, .. } | Self::Kind(kind) => text(kind, "correction kind"),
            Self::Field {
                name,
                fraction: value,
            } => {
                text(name, "correction field name")?;
                fraction(*value, "correction field fraction")
            }
            Self::Remove(RemoveStep::Tier(tier)) => text(tier, "correction remove tier"),
            Self::Remove(RemoveStep::Hover(Some([x, y]))) => {
                fraction(*x, "correction remove hover x")?;
                fraction(*y, "correction remove hover y")
            }
            Self::Remove(RemoveStep::Prompt(prompt)) if prompt.chars().count() > MAX_PROMPT => Err(
                format!("correction remove prompt holds at most {MAX_PROMPT} characters"),
            ),
            Self::Remove(_)
            | Self::Pin(_)
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

#[cfg(test)]
mod tests {
    use crate::{CorrectionStep, Step, parse, write};
    use serde_json::json;

    /// A paced press and drag round-trip with their interval, and an unpaced one writes none, so
    /// the scripts written before pacing are unchanged.
    #[test]
    fn paced_presses_and_drags_round_trip_and_unpaced_ones_write_no_interval() {
        let value = json!([
            {"correction":{"press":{"at":[0.3,0.4],"to":[[0.35,0.4]]}}},
            {"correction":{"press":{"at":[0.3,0.4],"to":[[0.35,0.4]],"interval_ms":24}}},
            {"correction":{"drag":{"handle":"source","to":[[0.5,0.5]]}}},
            {"correction":{"drag":{"handle":"source","to":[[0.5,0.5]],"interval_ms":24}}},
        ]);
        let steps = parse(&value.to_string()).unwrap();
        let Step::Correction(CorrectionStep::Press(press)) = &steps[1] else {
            panic!("a press: {:?}", steps[1]);
        };
        assert_eq!(press.interval_ms, Some(24));
        let Step::Correction(CorrectionStep::Drag(drag)) = &steps[3] else {
            panic!("a drag: {:?}", steps[3]);
        };
        assert_eq!(drag.interval_ms, Some(24));
        assert_eq!(write(&steps), value);
    }

    /// Every Remove control round-trips as it is written, and a bad one is refused.
    #[test]
    fn remove_steps_round_trip() {
        let value = json!([
            {"correction":{"kind":"remove"}},
            {"correction":{"remove":{"tier":"generative"}}},
            {"correction":{"remove":{"generate":"progress"}}},
            {"correction":{"remove":"cancel"}},
            {"correction":{"remove":{"generate":"done"}}},
            {"correction":{"remove":{"candidate":1}}},
            {"correction":{"remove":{"regenerate":"done"}}},
            {"correction":{"remove":{"prompt":"a red bicycle"}}},
            {"correction":{"remove":"setup"}},
            {"correction":{"remove":"accept"}},
        ]);
        let steps = parse(&value.to_string()).unwrap();
        assert_eq!(
            steps[2],
            Step::Correction(CorrectionStep::Remove(crate::RemoveStep::Generate(
                crate::RemoveUntil::Progress
            )))
        );
        assert_eq!(write(&steps), value);
        for refused in [
            json!({"correction":{"remove":{"tier":""}}}),
            json!({"correction":{"remove":{"prompt":"x".repeat(1025)}}}),
            json!({"correction":{"remove":{"generate":"soon"}}}),
        ] {
            assert!(parse(&json!([refused]).to_string()).is_err(), "{refused}");
        }
    }

    #[test]
    fn a_zero_interval_is_refused() {
        for step in [
            json!({"correction":{"press":{"at":[0.3,0.4],"interval_ms":0}}}),
            json!({"correction":{"drag":{"handle":"pin","to":[[0.5,0.5]],"interval_ms":0}}}),
        ] {
            let error = parse(&json!([step]).to_string()).unwrap_err();
            assert!(
                error.contains("interval_ms is a positive integer"),
                "{error}"
            );
        }
    }
}
