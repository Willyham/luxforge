//! Lightwell's widget library: the Develop workspace's visual language and generated controls.
//!
//! This crate depends on Iced only. It never depends on `lightwell-core`, and no test here links
//! it. That boundary is deliberate, for three reasons. First, it is enforced by the compiler
//! rather than by review: with no `lightwell-core` type in scope, nothing in this crate can reach
//! the catalog owner's authoritative state, validate a parameter against a module descriptor, or
//! construct an `edit.*` request — every widget here is therefore structurally unable to hold
//! editing logic, and takes plain data structs, enums and message values or closures instead.
//! Second, it is a compile-unit boundary: a styling or layout change here rebuilds this crate and
//! `lightwell-app`, never `lightwell-core`, so widget churn never lengthens the core's own build.
//! Third, the split is free at runtime: this crate links into the same desktop binary as
//! everything else and holds only pure functions and constants, so it starts no process, no
//! thread and no timer of its own — it adds no startup work beyond what any other Rust module
//! would.

pub mod geometry;
pub mod theme;
mod widgets;

pub use widgets::*;

mod gallery;

/// Builds one instance of every widget in every state shown on the components board
/// (`docs/design/develop-workspace/components.png`), as `Element<'_, ()>` values, so a caller can
/// prove the whole set builds without panicking.
///
/// Hidden because it exists for verification (today, the unit test in this crate; later, an
/// evidence renderer), not for reuse as part of the widget API.
#[doc(hidden)]
pub fn gallery_states() -> Vec<iced::Element<'static, ()>> {
    gallery::gallery()
}

#[cfg(test)]
mod tests {
    #[test]
    fn gallery_builds_every_widget_state_without_panicking() {
        let states = super::gallery_states();
        assert!(!states.is_empty());
    }
}
