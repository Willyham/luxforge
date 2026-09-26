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
    /// What a `preparation-required` refusal needs prepared, or the job preparing it. Boxed for the
    /// same reason as `data`.
    pub preparation: Option<Box<Preparation>>,
}
impl Error {
    pub fn new(kind: ErrorKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: detail.into(),
            data: None,
            preparation: None,
        }
    }
    /// One constructor per [`ErrorKind`], named after the kind, for every call site that knows its
    /// kind at compile time. `Error::new` stays for the few call sites where the kind is itself a
    /// variable.
    pub fn startup(detail: impl Into<String>) -> Self {
        Self::new(ErrorKind::Startup, detail)
    }
    pub fn unsupported_input(detail: impl Into<String>) -> Self {
        Self::new(ErrorKind::UnsupportedInput, detail)
    }
    pub fn unsupported_color(detail: impl Into<String>) -> Self {
        Self::new(ErrorKind::UnsupportedColor, detail)
    }
    pub fn unsupported_profile(detail: impl Into<String>) -> Self {
        Self::new(ErrorKind::UnsupportedProfile, detail)
    }
    pub fn file_access(detail: impl Into<String>) -> Self {
        Self::new(ErrorKind::FileAccess, detail)
    }
    pub fn decode(detail: impl Into<String>) -> Self {
        Self::new(ErrorKind::Decode, detail)
    }
    pub fn resource_limit(detail: impl Into<String>) -> Self {
        Self::new(ErrorKind::ResourceLimit, detail)
    }
    pub fn render(detail: impl Into<String>) -> Self {
        Self::new(ErrorKind::Render, detail)
    }
    pub fn cancelled(detail: impl Into<String>) -> Self {
        Self::new(ErrorKind::Cancelled, detail)
    }
    pub fn diagnostics(detail: impl Into<String>) -> Self {
        Self::new(ErrorKind::Diagnostics, detail)
    }
    pub fn validation(detail: impl Into<String>) -> Self {
        Self::new(ErrorKind::Validation, detail)
    }
    pub fn conflict(detail: impl Into<String>) -> Self {
        Self::new(ErrorKind::Conflict, detail)
    }
    pub fn catalog(detail: impl Into<String>) -> Self {
        Self::new(ErrorKind::Catalog, detail)
    }
    pub fn incompatible(detail: impl Into<String>) -> Self {
        Self::new(ErrorKind::Incompatible, detail)
    }
    pub fn source_unavailable(detail: impl Into<String>) -> Self {
        Self::new(ErrorKind::SourceUnavailable, detail)
    }
    pub fn preparation_required(detail: impl Into<String>) -> Self {
        Self::new(ErrorKind::PreparationRequired, detail)
    }
    pub fn consent_required(detail: impl Into<String>) -> Self {
        Self::new(ErrorKind::ConsentRequired, detail)
    }
    pub fn forbidden(detail: impl Into<String>) -> Self {
        Self::new(ErrorKind::Forbidden, detail)
    }
    pub fn not_ready(detail: impl Into<String>) -> Self {
        Self::new(ErrorKind::NotReady, detail)
    }
    pub fn protocol(detail: impl Into<String>) -> Self {
        Self::new(ErrorKind::Protocol, detail)
    }
    pub fn internal(detail: impl Into<String>) -> Self {
        Self::new(ErrorKind::Internal, detail)
    }
    /// The same error carrying structured data for the client.
    pub fn with_data(mut self, data: serde_json::Value) -> Self {
        self.data = Some(Box::new(data));
        self
    }
    /// The same error naming what it needs prepared, or the job preparing it.
    pub fn with_preparation(mut self, preparation: Preparation) -> Self {
        self.preparation = Some(Box::new(preparation));
        self
    }
    /// What this refusal needs prepared, when the work that was refused named it and nothing has
    /// been queued for it yet.
    pub fn needs(&self) -> Option<&PreparationNeeds> {
        match self.preparation.as_deref() {
            Some(Preparation::Needs(needs)) => Some(needs),
            _ => None,
        }
    }
    /// The source job preparing what this refusal needs: wait for it, then ask again.
    pub fn preparation_job(&self) -> Option<&crate::JobId> {
        match self.preparation.as_deref() {
            Some(Preparation::Queued(job)) => Some(job),
            _ => None,
        }
    }
}

/// What a `preparation-required` refusal carries: what the refused work needs prepared, as the
/// service that evaluated the stack named it, or, once the catalog owner has queued that, the job
/// to wait for.
#[derive(Clone, Debug, PartialEq)]
pub enum Preparation {
    Needs(PreparationNeeds),
    Queued(crate::JobId),
}

/// Everything one evaluated stack needs prepared before it can be evaluated, named where the stack
/// is known, so the catalog owner queues exactly this as one source job and never re-derives it
/// from the request that was refused. Preparing it always includes the asset's verified original.
#[derive(Clone, Debug, PartialEq)]
pub struct PreparationNeeds {
    /// The asset whose original the stack reads.
    pub asset_id: crate::AssetId,
    /// The saved entry that was evaluated, or the one a draft or a change was planned over.
    pub entry_id: crate::EntryId,
    /// The sensor gains a RAW original's development must hold, or `None` for a JPEG: the
    /// evaluated stack's own white balance, except for a drafted preview, which approximates its
    /// white balance on the development its entry holds and so names that one.
    pub gains: Option<[f32; 3]>,
    /// The derived artifacts the stack references that are not ready.
    pub artifacts: Vec<crate::ArtifactId>,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.kind.code(), self.detail)
    }
}
impl std::error::Error for Error {}
