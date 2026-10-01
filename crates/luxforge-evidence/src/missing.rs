//! Evidence steps on Select's Missing originals: each gesture sent through the message
//! its control sends, the native dialogs bypassed with the path the step names, and captured once
//! nothing Missing originals asked the owner for is in flight.
use super::{is_false, text};
use serde::{Deserialize, Serialize};

/// One gesture on Missing originals.
///
/// `{"find": {"group": "2026-09 Konstanz", "folder": "/path"}}` presses Find in a folder… on the
/// group developed from the folder of that name, answering the folder dialog with `folder`, and is
/// captured once the search has ended; with `"stop": true`, Stop search is pressed as soon as the
/// search has started, and the frame is captured once its job has ended. `{"filter": "needs_you"}`
/// presses a filter segment. `{"row": "DSC_0004.JPG"}` presses the row of the photograph whose
/// original has that file name, which the Info panel then describes. `{"choose": {"file":
/// "DSC_0004.JPG", "index": 1}}` opens that row's Choose… menu and chooses its file at `index`,
/// from 0, as the menu lists them. `"relink"` presses Relink N and is captured once the list has
/// been read again. `{"locate": {"file": "DSC_0007.JPG", "path": "/path"}}` presses that row's
/// Locate…, answering the file dialog with `path`, and is captured once the Locate has ended and
/// the list has been read again.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum MissingStep {
    Find {
        group: String,
        folder: String,
        #[serde(default, skip_serializing_if = "is_false")]
        stop: bool,
    },
    Filter(MissingFilterStep),
    Row(String),
    Choose {
        file: String,
        index: usize,
    },
    Relink,
    Locate {
        file: String,
        path: String,
    },
}

/// A filter segment of Missing originals.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MissingFilterStep {
    All,
    Found,
    NeedsYou,
    NotFound,
}

impl MissingStep {
    pub(super) fn validate(&self) -> Result<(), String> {
        match self {
            Self::Find { group, folder, .. } => {
                text(group, "missing find group")?;
                text(folder, "missing find folder")
            }
            Self::Row(file) | Self::Choose { file, .. } => text(file, "missing row file"),
            Self::Locate { file, path } => {
                text(file, "missing locate file")?;
                text(path, "missing locate path")
            }
            Self::Filter(_) | Self::Relink => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{Step, parse};
    use serde_json::json;

    /// Every Missing originals step is spelled as the scenario writes it and read back the same.
    #[test]
    fn missing_steps_round_trip() {
        let script = json!([
            {"missing": {"find": {"group": "2026-09 Konstanz", "folder": "/tmp/Archive"}}},
            {"missing": {"find": {"group": "2026-09 Lake", "folder": "/tmp", "stop": true}}},
            {"missing": {"filter": "needs_you"}},
            {"missing": {"row": "DSC_0004.JPG"}},
            {"missing": {"choose": {"file": "DSC_0004.JPG", "index": 1}}},
            {"missing": "relink"},
            {"missing": {"locate": {"file": "DSC_0007.JPG", "path": "/tmp/DSC_0007.JPG"}}},
        ]);
        let steps = parse(&script.to_string()).unwrap();
        assert_eq!(steps.len(), 7);
        let written = serde_json::Value::Array(steps.iter().map(Step::to_value).collect());
        assert_eq!(written, script);
        assert!(parse(&json!([{"missing": {"row": " "}}]).to_string()).is_err());
        assert!(parse(&json!([{"missing": {"find": {"group": "a"}}}]).to_string()).is_err());
    }
}
