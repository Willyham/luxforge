//! The Select workspace.

/// Browsing files and the catalog, picking and developing in the Select workspace, and the owner's
/// answers. Handled in `app/select.rs`. **Lane D (views and desktop)** adds its variants: the
/// workspace switch, sources, the grid and the loupe, picks, Develop N and its confirmation, the
/// library's undo and long-running work.
#[derive(Clone, Debug)]
pub(crate) enum SelectMessage {}
