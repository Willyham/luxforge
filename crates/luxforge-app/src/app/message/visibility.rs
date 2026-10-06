//! Native window facts; independent of focus, recipes and persisted preferences.
#[derive(Clone, Debug)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) enum VisibilityMessage {
    Pending,
    Installed(Result<luxforge_input::Visibility, String>),
}
