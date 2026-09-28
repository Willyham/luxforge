//! The headless command line: the `luxforge-json` binary, which serves one JSON-lines client over
//! its standard streams, and [`Paths`], where the application keeps its files, which that binary
//! and the desktop resolve the same way. It depends on no GUI crate, so building the headless
//! binary builds no window, renderer or dialog stack.
mod paths;

pub use paths::Paths;
