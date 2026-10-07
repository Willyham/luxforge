//! Evidence steps on developing picks and Develop's development set: Develop N's confirmation in
//! Select, opening a catalog photograph, and moving through the filmstrip in Develop. Each
//! gesture is sent through the message its control or key sends.
use super::text;
use serde::{Deserialize, Serialize};

/// One gesture on developing picks or the development set.
///
/// `"picks"` presses Select's Develop N, and is captured once the confirmation shows what
/// `pick.plan` answered. `"active"` double-clicks the active photograph in a catalog view in Select:
/// it opens Develop on that photograph with the view's photographs as the development set,
/// captured once that photograph's exact render is on screen.
/// `{"name": {"event": 0, "text": "Lake trip"}}` types `text` over the
/// selected name of the confirmation's event at that index, as typing does. `{"existing":
/// {"event": 0, "folder": "Konstanz · Sep 2026"}}` opens that event's Or add to an existing folder
/// and chooses the folder listed so. `{"copies": false}` sets whether a card's picks use their
/// copies in indexed folders. `"confirm"` presses Develop (Return), and is captured once the Develop
/// has ended and Develop shows the first developed photograph's exact render, or the reason it
/// did not. `"cancel"` is Escape, captured at once.
///
/// In Develop: `{"step": "next"}` or `{"step": "previous"}` is `→` or `←`, and `{"cell": 3}` presses
/// the filmstrip's cell of the set's photograph at that index; each is captured in the frame after
/// the key, which draws the photograph's cached large preview when one is decoded. `"settle"` is
/// captured once the photograph moved to is open and its exact render has replaced the preview.
/// `"ready"` is captured once the large previews Develop decodes ahead — the active photograph's
/// neighbours in the set — are decoded. `"strip"` is `Cmd+Option+F`, which collapses or expands the
/// filmstrip.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum DevelopStep {
    Picks,
    Active,
    Name { event: usize, text: String },
    Existing { event: usize, folder: String },
    Copies(bool),
    Confirm,
    Cancel,
    Step(SetStep),
    Cell(usize),
    Settle,
    Ready,
    Strip,
}

/// A step through the development set.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SetStep {
    Previous,
    Next,
}

impl DevelopStep {
    pub(super) fn validate(&self) -> Result<(), String> {
        match self {
            Self::Name { text: name, .. } => text(name, "develop folder name"),
            Self::Existing { folder, .. } => text(folder, "develop existing folder"),
            Self::Picks
            | Self::Active
            | Self::Copies(_)
            | Self::Confirm
            | Self::Cancel
            | Self::Step(_)
            | Self::Cell(_)
            | Self::Settle
            | Self::Ready
            | Self::Strip => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{Step, parse};
    use serde_json::json;

    /// Every develop step is spelled as the scenarios write it and read back the same.
    #[test]
    fn develop_steps_round_trip() {
        let script = json!([
            {"develop": "picks"},
            {"develop": "active"},
            {"develop": {"name": {"event": 0, "text": "Lake trip"}}},
            {"develop": {"existing": {"event": 1, "folder": "Konstanz \u{b7} Sep 2026"}}},
            {"develop": {"copies": false}},
            {"develop": "confirm"},
            {"develop": "cancel"},
            {"develop": {"step": "next"}},
            {"develop": {"step": "previous"}},
            {"develop": {"cell": 3}},
            {"develop": "settle"},
            {"develop": "ready"},
            {"develop": "strip"},
        ]);
        let steps = parse(&script.to_string()).unwrap();
        assert_eq!(steps.len(), 13);
        let written = serde_json::Value::Array(steps.iter().map(Step::to_value).collect());
        assert_eq!(written, script);
        assert!(
            parse(&json!([{"develop": {"name": {"event": 0, "text": " "}}}]).to_string()).is_err()
        );
        assert!(parse(&json!([{"develop": {"step": "up"}}]).to_string()).is_err());
    }
}
