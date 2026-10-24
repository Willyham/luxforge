//! The headless command lines and what they share with the desktop: the `luxforge-json` binary,
//! which serves one JSON-lines client over its standard streams; the `luxforge-ctl` binary, a
//! client of the desktop's live session ([`live`]); the MCP adapter's tool schemas and request
//! check, generated from `schema.list` ([`mcp`]); and [`Paths`], where the application keeps its
//! files, which the desktop and these binaries resolve the same way. It depends on no GUI crate, so
//! building the headless binaries builds no window, renderer or dialog stack.
pub mod live;
pub mod mcp;
mod paths;

pub use paths::Paths;

/// The application version every executable's `--version` reports,
/// [`luxforge_core::APPLICATION_VERSION`]: the release tag's normalized version in a tag build,
/// otherwise the workspace version.
pub const VERSION: &str = luxforge_core::APPLICATION_VERSION;

/// A standalone version request has no catalog, network, Keychain or desktop side effects.
pub fn version_requested(mut args: impl Iterator<Item = std::ffi::OsString>) -> bool {
    args.next().as_deref() == Some(std::ffi::OsStr::new("--version")) && args.next().is_none()
}

#[cfg(test)]
mod tests {
    #[test]
    fn only_a_lone_version_flag_is_a_version_request() {
        let requested = |args: &[&str]| super::version_requested(args.iter().map(Into::into));
        assert!(requested(&["--version"]));
        for args in [
            &[][..],
            &["--version", "--catalog"],
            &["--catalog", "--version"],
            &["-V"],
            &["version"],
        ] {
            assert!(!requested(args), "{args:?}");
        }
    }
}
