//! The headless command lines and what they share with the desktop: the `luxforge-json` binary,
//! which serves one JSON-lines client over its standard streams; the `luxforge-ctl` binary, a
//! client of the desktop's live session ([`live`]); [`Paths`], where the application keeps its
//! files; and [`CatalogSelection`], which catalog an ordinary launch opens. The desktop and these
//! binaries resolve paths and catalogs the same way. It depends on no GUI crate, so building the
//! headless binaries builds no window, renderer or dialog stack.
mod catalog;
pub mod live;
mod paths;

pub use catalog::{CATALOG_FILE, CatalogSelection};
pub use paths::Paths;
