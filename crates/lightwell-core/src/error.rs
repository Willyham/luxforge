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
    /// A gated module operation has no matching grant. The error's data carries the exact scope and
    /// the disclosure a person needs to decide, so a headless client can ask the same question the
    /// desktop does.
    ConsentRequired,
    /// The client lacks the authority the method needs, such as granting a permission.
    Forbidden,
    /// A declared requirement is not met yet: a setting, a resource, an activation or the secure
    /// store. The error's data lists what is missing; nothing was queued.
    NotReady,
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
            Self::ConsentRequired => "consent-required",
            Self::Forbidden => "forbidden",
            Self::NotReady => "not-ready",
            Self::Protocol => "protocol",
            Self::Internal => "internal",
        }
    }
}
#[derive(Debug, Clone)]
pub struct Error {
    pub kind: ErrorKind,
    pub detail: String,
    /// Structured context a client acts on, such as a consent request's scope or the requirements a
    /// module is missing. Never a secret. Boxed so a `Result` stays small on the common path.
    pub data: Option<Box<serde_json::Value>>,
}
impl Error {
    pub fn new(kind: ErrorKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: detail.into(),
            data: None,
        }
    }
    /// The same error carrying structured data for the client.
    pub fn with_data(mut self, data: serde_json::Value) -> Self {
        self.data = Some(Box::new(data));
        self
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.kind.code(), self.detail)
    }
}
impl std::error::Error for Error {}
