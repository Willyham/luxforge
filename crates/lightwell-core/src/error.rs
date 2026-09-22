//! Stable internal error categories shared by GUI and development drivers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    Startup,
    UnsupportedInput,
    UnsupportedColor,
    UnsupportedProfile,
    FileAccess,
    Decode,
    ResourceLimit,
    Render,
    /// A render, resample, colour pass or reduction that a newer request superseded. It is not a
    /// failure of the work: nothing was wrong with the recipe, the source or the budget, and the
    /// caller that cancelled already knows why. No partial frame or report accompanies it.
    Cancelled,
    Diagnostics,
    Validation,
    Conflict,
    Catalog,
    Incompatible,
    SourceUnavailable,
    PreparationRequired,
    Protocol,
    Internal,
}
impl ErrorKind {
    pub fn code(self) -> &'static str {
        match self {
            Self::Startup => "startup",
            Self::UnsupportedInput => "unsupported-input",
            Self::UnsupportedColor => "unsupported-color",
            Self::UnsupportedProfile => "unsupported-profile",
            Self::FileAccess => "read-error",
            Self::Decode => "invalid-input",
            Self::ResourceLimit => "resource-limit",
            Self::Render => "render",
            Self::Cancelled => "cancelled",
            Self::Diagnostics => "diagnostics",
            Self::Validation => "validation",
            Self::Conflict => "conflict",
            Self::Catalog => "catalog",
            Self::Incompatible => "incompatible",
            Self::SourceUnavailable => "source-unavailable",
            Self::PreparationRequired => "preparation-required",
            Self::Protocol => "protocol",
            Self::Internal => "internal",
        }
    }
}
#[derive(Debug, Clone)]
pub struct Error {
    pub kind: ErrorKind,
    pub detail: String,
}
impl Error {
    pub fn new(kind: ErrorKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: detail.into(),
        }
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.kind.code(), self.detail)
    }
}
impl std::error::Error for Error {}
