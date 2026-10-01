//! Evidence steps on the catalog in Select: each gesture sent through the message its
//! control sends — a source row, the search field, the Metadata browser, a chip's menu, a folder's
//! menu, the Info panel's Move to… and Add to…, Save as smart collection…, Apply preset… and
//! Export…, Remove from catalog…, Put back and Empty Removed… with their confirmations — and
//! captured once nothing Select asked the owner for is in flight, a batch or an emptying ended.
use super::text;
use serde::{Deserialize, Serialize};

/// One gesture on the catalog.
///
/// `{"source": "Konstanz · Sep 2026"}` presses the catalog folder, year or collection row of that
/// name (a year opens or closes). `{"search": "Luzern"}` types that text into the search field.
/// `"metadata"` presses the Metadata chip. `{"facet": {"column": "camera", "value": "iPhone 15
/// Pro"}}` presses the Metadata browser's value of that label. `{"edited": "Not edited"}` chooses
/// from the Edited chip's menu. `{"rename": {"folder": "A", "name": "B"}}` chooses Rename… from
/// the folder's menu, types the name and presses Return; `{"nest": {"folder": "A", "into": "B"}}`
/// its Move to… and the folder listed as `B` (or `Top level`); `{"merge": {"folder": "A", "into":
/// "B"}}` its Merge into…. `{"new_folder": "A"}` presses the Catalog heading's `+`, New folder,
/// types the name and presses Return. `{"move_to": "B"}` and `{"add_to": "Print order"}` press the
/// Info panel's Move to… and Add to… and choose the folder or collection listed so, for the
/// selection. `{"save_smart": "Name"}` presses Save as smart collection…, types the name and
/// presses Return.
///
/// `{"apply_preset": "Warm"}` opens the Develop band's Apply preset… and chooses the library preset
/// of that name for the selection; `{"export_into": "/path"}` presses Export… and answers the native
/// folder dialog with that folder: each is captured once its batch has ended and what it changed
/// has been read again. `"report"` presses the status bar's Report beside the batch's sentence.
/// `"remove"` presses the Info panel's Remove from catalog…, `"delete_key"` presses ⌫ through the
/// key table, `"empty_removed"` the filter bar's Empty Removed… over Removed: each is captured with
/// its confirmation. `"confirm"` presses the confirmation's own button. `"put_back"` presses the
/// Info panel's Put back over Removed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum CatalogStep {
    Source(String),
    Search(String),
    Metadata,
    Facet { column: FacetColumn, value: String },
    Edited(String),
    Rename { folder: String, name: String },
    Nest { folder: String, into: String },
    Merge { folder: String, into: String },
    NewFolder(String),
    MoveTo(String),
    AddTo(String),
    SaveSmart(String),
    ApplyPreset(String),
    ExportInto(String),
    Report,
    Remove,
    DeleteKey,
    Confirm,
    PutBack,
    EmptyRemoved,
}

/// A column of the Metadata browser.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FacetColumn {
    Date,
    Place,
    Camera,
    Lens,
}

impl CatalogStep {
    pub(super) fn validate(&self) -> Result<(), String> {
        match self {
            Self::Source(name) => text(name, "catalog source"),
            Self::Search(_) | Self::Metadata => Ok(()),
            Self::Facet { value, .. } => text(value, "catalog facet value"),
            Self::Edited(item) => text(item, "catalog edited choice"),
            Self::Rename { folder, name } => {
                text(folder, "catalog rename folder")?;
                text(name, "catalog rename name")
            }
            Self::Nest { folder, into } | Self::Merge { folder, into } => {
                text(folder, "catalog folder")?;
                text(into, "catalog folder it goes into")
            }
            Self::NewFolder(name) => text(name, "catalog new folder"),
            Self::MoveTo(name) => text(name, "catalog move to"),
            Self::AddTo(name) => text(name, "catalog add to"),
            Self::SaveSmart(name) => text(name, "catalog smart collection name"),
            Self::ApplyPreset(name) => text(name, "catalog preset"),
            Self::ExportInto(folder) => text(folder, "catalog export folder"),
            Self::Report
            | Self::Remove
            | Self::DeleteKey
            | Self::Confirm
            | Self::PutBack
            | Self::EmptyRemoved => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{Step, parse};
    use serde_json::json;

    /// Every catalog step is spelled as the scenario writes it and read back the same.
    #[test]
    fn catalog_steps_round_trip() {
        let script = json!([
            {"catalog": {"source": "Real photographs"}},
            {"catalog": {"search": "Luzern"}},
            {"catalog": "metadata"},
            {"catalog": {"facet": {"column": "camera", "value": "iPhone 15 Pro"}}},
            {"catalog": {"edited": "Not edited"}},
            {"catalog": {"rename": {"folder": "Real photographs", "name": "Swiss trip"}}},
            {"catalog": {"nest": {"folder": "Swiss trip", "into": "Travel"}}},
            {"catalog": {"merge": {"folder": "A", "into": "B"}}},
            {"catalog": {"new_folder": "Lakes"}},
            {"catalog": {"move_to": "Bodensee"}},
            {"catalog": {"add_to": "Print order"}},
            {"catalog": {"save_smart": "iPhone in Luzern"}},
            {"catalog": {"apply_preset": "Warm"}},
            {"catalog": {"export_into": "/tmp/batch-export"}},
            {"catalog": "report"},
            {"catalog": "remove"},
            {"catalog": "delete_key"},
            {"catalog": "confirm"},
            {"catalog": "put_back"},
            {"catalog": "empty_removed"},
        ]);
        let steps = parse(&script.to_string()).unwrap();
        assert_eq!(steps.len(), 20);
        let written = serde_json::Value::Array(steps.iter().map(Step::to_value).collect());
        assert_eq!(written, script);
        assert!(parse(&json!([{"catalog": {"source": " "}}]).to_string()).is_err());
        assert!(parse(&json!([{"catalog": {"rename": {"folder": "a"}}}]).to_string()).is_err());
        assert!(parse(&json!([{"catalog": {"apply_preset": " "}}]).to_string()).is_err());
    }
}
