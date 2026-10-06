//! Remembered state: the workspace switches, the brush and the export folder the desktop keeps
//! across launches with no Settings row of their own
//! ([design](../../../../docs/design/preferences.md#behaviour)).
//!
//! At launch the session and the Masks panel's brush start from the stored values; afterwards each
//! change the person makes is stored through the one preference writer
//! ([`Editor::store_preferences`]), which keeps one call in flight and merges the rest, so a held
//! key writes at most one call plus one waiting. A change is stored only when it differs from what
//! the desktop already shows of the preference, so the launch's own seeding writes nothing.
use super::{Editor, message::Message, tasks};
use crate::{
    mask_draft::{Brush, NEUTRAL_BRUSH},
    state::preferences::PreferenceChange,
};
use iced::Task;
use luxforge_core::{
    ClientSession, WorkspaceState,
    preferences::{BrushPreference, WorkspacePreference},
};
use serde_json::json;
use std::path::{Path, PathBuf};

/// The five workspace switches a session holds that the desktop remembers. The canvas mode, the
/// mask overlay mode and zoom are left out: they are not remembered.
pub(crate) fn remembered_workspace(workspace: &WorkspaceState) -> WorkspacePreference {
    WorkspacePreference {
        state_panel: workspace.state_panel,
        tools_panel: workspace.tools_panel,
        thirds: workspace.thirds,
        clip_shadows: workspace.clip_shadows,
        clip_highlights: workspace.clip_highlights,
    }
}

/// The brush numbers the desktop remembers. Erase, Limit to colour and the colour refine belong to
/// the stroke and are left out.
pub(crate) fn remembered_brush(brush: &Brush) -> BrushPreference {
    BrushPreference {
        size: brush.size,
        feather: brush.feather,
        flow: brush.flow,
    }
}

/// The brush the Masks panel starts with: the stored numbers over the neutral brush, or the neutral
/// brush when none is stored.
pub(crate) fn starting_brush(stored: Option<BrushPreference>) -> Brush {
    match stored {
        Some(stored) => Brush {
            size: stored.size,
            feather: stored.feather,
            flow: stored.flow,
            ..NEUTRAL_BRUSH
        },
        None => NEUTRAL_BRUSH,
    }
}

impl Editor {
    /// Start this launch from the remembered preferences, before the first frame: the Masks
    /// panel's brush, and the session's five remembered switches and mask overlay colour through
    /// one `workspace.set`, adopted at once. The call is made while the editor is built, as the
    /// launch's `preferences.read` is, so the first frame already shows the panels and overlays.
    /// It is made only when the stored values differ from the session's defaults, so a launch
    /// with nothing stored, such as every evidence run's, sends nothing. Nothing is written back:
    /// the session now matches what is stored.
    pub(super) fn seed_remembered(&mut self) {
        let Some(preferences) = self.preferences.applied() else {
            return;
        };
        self.mask_panel.brush = starting_brush(preferences.brush);
        let held = &self.session.workspace;
        let remembered = preferences.workspace;
        if remembered_workspace(held) == remembered
            && held.mask_overlay_colour == preferences.mask_overlay_colour
        {
            return;
        }
        let params = json!({
            "state_panel": remembered.state_panel,
            "tools_panel": remembered.tools_panel,
            "thirds": remembered.thirds,
            "clip_shadows": remembered.clip_shadows,
            "clip_highlights": remembered.clip_highlights,
            "mask_overlay_colour": preferences.mask_overlay_colour.as_str(),
        });
        let answer = tasks::call(&self.owner, self.client, "workspace.set", params).and_then(
            |(session, _)| {
                serde_json::from_value::<ClientSession>(session).map_err(|e| e.to_string())
            },
        );
        match answer {
            Ok(session) => self.adopt(session),
            Err(reason) => {
                self.status.text = format!("Could not restore the workspace: {reason}");
                self.event("workspace_restore_failed", || json!({"reason": reason}));
            }
        }
    }

    /// A `workspace.set` answer was adopted: when it changed any of the five remembered switches
    /// from `before`, and they now differ from what the preference shows, store them. A change of
    /// mode or mask overlay leaves them as they were and stores nothing.
    pub(super) fn remember_workspace(&mut self, before: WorkspacePreference) -> Task<Message> {
        let now = remembered_workspace(&self.session.workspace);
        if now == before {
            return Task::none();
        }
        match self.preferences.applied() {
            Some(preferences) if preferences.workspace != now => {
                self.store_preferences(PreferenceChange {
                    workspace: Some(now),
                    ..PreferenceChange::default()
                })
            }
            _ => Task::none(),
        }
    }

    /// The Masks panel's brush may have changed: when its size, feather or flow now differ from
    /// what the preference shows — the neutral brush while none is stored — store them. A reset
    /// stores the neutral value; erase, Limit to colour and the colour refine store nothing.
    pub(super) fn remember_brush(&mut self) -> Task<Message> {
        let now = remembered_brush(&self.mask_panel.brush);
        let Some(preferences) = self.preferences.applied() else {
            return Task::none();
        };
        let shown = preferences
            .brush
            .unwrap_or_else(|| remembered_brush(&NEUTRAL_BRUSH));
        if shown == now {
            return Task::none();
        }
        self.store_preferences(PreferenceChange {
            brush: Some(Some(now)),
            ..PreferenceChange::default()
        })
    }

    /// An export whose destination the person chose in the save dialog succeeded: store the
    /// destination's folder, which `export.plan` then suggests for the next export. `folder` is
    /// `None` for a destination given any other way, which stores nothing.
    pub(super) fn remember_export_folder(&mut self, folder: Option<PathBuf>) -> Task<Message> {
        let Some(folder) = folder.filter(|folder| folder.is_absolute()) else {
            return Task::none();
        };
        match self.preferences.applied() {
            Some(preferences) if preferences.export_folder.as_deref() != Some(&folder) => self
                .store_preferences(PreferenceChange {
                    export_folder: Some(Some(folder)),
                    ..PreferenceChange::default()
                }),
            _ => Task::none(),
        }
    }
}

/// The folder a chosen destination is in, as the remembered export folder stores it.
pub(crate) fn export_folder(destination: &Path) -> Option<PathBuf> {
    destination
        .parent()
        .filter(|folder| !folder.as_os_str().is_empty())
        .map(Path::to_path_buf)
}
