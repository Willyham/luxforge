//! The per-setting import report: what was carried, what was neutral, what Luxforge cannot
//! reproduce and what it refused, one entry per setting the file holds.
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Every setting of an imported file except preset metadata, in exactly one of four lists, each
/// sorted by setting name.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportReport {
    /// `luxforge`, `lightroom-xmp` or `lightroom-template`.
    pub format: String,
    /// The file's `ProcessVersion` as written, when it has one.
    #[serde(default)]
    pub process_version: Option<String>,
    /// Value transfers now in the preset's settings.
    pub mapped: Vec<MappedSetting>,
    /// Settings Luxforge has no tool for, at a value that changes nothing, so nothing is lost.
    pub neutral: Vec<ReportedSetting>,
    /// Effects Luxforge does not reproduce, because it has no such tool.
    pub unsupported: Vec<ReportedSetting>,
    /// Values Luxforge has a related control for but cannot carry: out of range, from another
    /// process version, disabled in the preset or without a calibrated conversion.
    pub refused: Vec<ReportedSetting>,
    /// Values the file implies as a whole rather than holds as settings, so they are in none of
    /// the four lists and no count: a split-toning document's Blending 100 and the controls Color
    /// Grading added at 0. Listed only when there is one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub derived: Vec<DerivedSettings>,
}

/// Fields an import writes because of what the whole file is, not because of one of its settings.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DerivedSettings {
    /// Why the file implies these values.
    pub reason: String,
    /// The values, as a settings set holds them.
    pub settings: Map<String, Value>,
    /// Why the values are not in the preset's settings, when they are not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refused: Option<String>,
}

/// One value transfer: the same number on the Luxforge control with the same name, range and
/// direction. It is not a claim that Luxforge renders what Lightroom renders.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MappedSetting {
    pub setting: String,
    /// The text as written, at most 256 characters.
    pub value: String,
    pub action: String,
    /// None for a recomputed analysis step, whose applied value is its empty parameters.
    pub field: Option<String>,
    /// The value the field receives.
    pub applied: Value,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReportedSetting {
    pub setting: String,
    /// The text as written; a curve or a sequence is its items joined with `; `. At most 256
    /// characters.
    pub value: String,
    /// Why the setting is unsupported or refused. A neutral setting has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// The size of each list, for a library listing and a status line.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReportCounts {
    pub mapped: usize,
    pub neutral: usize,
    pub unsupported: usize,
    pub refused: usize,
}

impl ImportReport {
    pub(crate) fn counts(&self) -> ReportCounts {
        ReportCounts {
            mapped: self.mapped.len(),
            neutral: self.neutral.len(),
            unsupported: self.unsupported.len(),
            refused: self.refused.len(),
        }
    }

    /// Whether some effect of the file is not reproduced: anything unsupported or refused.
    #[cfg(test)]
    pub(crate) fn partial(&self) -> bool {
        !self.unsupported.is_empty()
            || !self.refused.is_empty()
            || self.derived.iter().any(|derived| derived.refused.is_some())
    }
}

impl std::fmt::Display for ReportCounts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} mapped, {} neutral, {} unsupported, {} refused",
            self.mapped, self.neutral, self.unsupported, self.refused
        )
    }
}
