//! The status codes the native adapter's calls return. The build script writes this table into
//! the adapter's generated header as `enum LfStatus` (`LF_STATUS_OK`, `LF_STATUS_INVALID_INPUT`
//! and so on), so both languages take each code from here.

/// The outcome of one native call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub(crate) enum NativeStatus {
    Ok = 0,
    /// Missing or mismatched arguments or buffers.
    InvalidInput = 1,
    /// The caller's cancel callback asked to stop.
    Cancelled = 2,
    /// The decoder or the demosaic failed, a tile job or the executor failed, or C++ threw.
    Failed = 3,
    /// Dimensions, stride or a black pattern past the adapter's bounds, or a frame below the
    /// demosaic's minimum.
    Geometry = 4,
    /// A colour filter array or sample layout the adapter does not develop.
    UnsupportedCfa = 5,
    /// A native allocation failed.
    Allocation = 6,
    /// A camera or frame count outside the catalog.
    UnsupportedMode = 7,
    /// LibRaw chose its Nikon High Efficiency decoder, which reads nothing.
    NikonHighEfficiency = 8,
}

impl NativeStatus {
    /// Every status, in code order.
    pub(crate) const ALL: [Self; 9] = [
        Self::Ok,
        Self::InvalidInput,
        Self::Cancelled,
        Self::Failed,
        Self::Geometry,
        Self::UnsupportedCfa,
        Self::Allocation,
        Self::UnsupportedMode,
        Self::NikonHighEfficiency,
    ];
}
