//! The headless command lines and what they share with the desktop: the `luxforge-json` binary,
//! which serves one JSON-lines client over its standard streams; the `luxforge-ctl` binary, a
//! client of the desktop's live session ([`live`]); and [`Paths`], where the application keeps its
//! files, which the desktop and these binaries resolve the same way. It depends on no GUI crate, so
//! building the headless binaries builds no window, renderer or dialog stack.
pub mod live;
mod paths;

pub use paths::Paths;
