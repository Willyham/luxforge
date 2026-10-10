//! Optional usage and performance statistics: protected consent, typed events and their bounded
//! delivery (`docs/design/usage-statistics.md`). A host service, not an editing module: nothing here
//! reads an original, a pixel or a recipe value, and nothing runs until a person opts in.
pub mod consent;
pub mod device_profile;
pub mod eligibility;
pub mod encode;
pub mod event;
pub mod gate;
pub mod image_id;
pub(crate) mod produce;
pub mod registry;

pub use device_profile::{
    AppleTier, Arch, CpuProfile, CpuVendor, DeviceProfile, GpuBackend, GpuProfile, GpuType,
    GpuVendor, LinuxDistro, MacBuild, MemoryBand, OsFamily, OsProfile, ProfileError,
    RendererFallback, Version,
};
pub use encode::{
    Batch, BodyBuffer, EncodeError, Encoded, Envelope, Release, ReportId, SchemaError,
    SessionStart, decode_batch, encode_batch,
};
pub use event::{
    AddedCount, Collector, Detached, Diagnostics, DurationMs, Event, EventKind, MemoryMib,
    OperationEnd, Percent, ProcessSample, Recorded, Session, SessionClock, Window,
};
pub use gate::{Gate, Generation};
pub use image_id::{ImageId, ImageKey};
pub use registry::{
    EventName, Feature, Idle, MemoryKind, Operation, Outcome, Presented, Renderer, Tool, Trigger,
    Workspace,
};

/// The disclosure the startup notice and Settings › Privacy describe. Consent saved for another
/// version is not consent to this one.
pub const DISCLOSURE_VERSION: u32 = 1;

/// The only production destination. Tests inject their own receiver.
pub const DESTINATION: &str = "https://api.luxforge.app/v1/usage/batches";
