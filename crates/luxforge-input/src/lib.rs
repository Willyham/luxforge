//! Native input missing from the window framework, behind a safe, numerical callback.
//! No recipe, catalog, image or GUI-framework dependency belongs here.

/// One trackpad magnification increment, at a top-left-origin content-view position in points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pinch {
    pub delta: f64,
    pub x: f64,
    pub y: f64,
}

#[cfg(target_os = "macos")]
mod macos;

/// Install for this window on the main thread, from the window runtime. Other windows and native
/// dialogs are ignored. Replacement removes the old
/// monitor; the main thread owns the new one until replacement or exit. Events still reach AppKit.
#[cfg(target_os = "macos")]
pub use macos::install_pinch_handler;
