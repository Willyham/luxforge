//! The headless command lines and what they share with the desktop: the `luxforge-json` binary,
//! which serves one JSON-lines client over its standard streams; the `luxforge-ctl` binary, a
//! client of the desktop's live session ([`live`]); and [`Paths`], where the application keeps its
//! files, which the desktop and these binaries resolve the same way. It depends on no GUI crate, so
//! building the headless binaries builds no window, renderer or dialog stack.
pub mod live;
mod paths;

pub use paths::Paths;

/// The release tag supplies the application version at compile time. Ordinary builds use the
/// workspace version. Cargo tracks this environment macro when deciding whether to rebuild.
pub const VERSION: &str = match option_env!("LUXFORGE_RELEASE_VERSION") {
    Some(version) => version,
    None => env!("CARGO_PKG_VERSION"),
};

/// A standalone version request has no catalog, network, Keychain or desktop side effects.
pub fn version_requested(mut args: impl Iterator<Item = std::ffi::OsString>) -> bool {
    args.next().as_deref() == Some(std::ffi::OsStr::new("--version")) && args.next().is_none()
}
