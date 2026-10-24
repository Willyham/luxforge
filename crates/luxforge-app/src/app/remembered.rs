//! Remembered state: the workspace switches, the brush, the last export and the Develop folder the
//! desktop keeps across launches with no Settings row of their own
//! ([design](../../../../docs/design/preferences.md#behaviour)).
//!
//! At launch the session and the Masks panel's brush start from the stored values; afterwards each
//! change the person makes is stored through the one preference writer
//! ([`Editor::store_preferences`]), which keeps one call in flight and merges the rest, so a held
//! key writes at most one call plus one waiting. A change is stored only when it differs from what
//! the desktop already shows of the preference, so the launch's own seeding writes nothing.
//!
//! The Develop folder ([develop from
//! folders](../../../../docs/design/develop-from-folders.md#develops-folder)) is remembered with
//! the identity of the catalog holding it (`catalog.info`'s `catalog_id`). Every launch reads that
//! identity, and `folder.list` when the remembered folder is this catalog's, in one owner task
//! ([`remembered_folder_now`]). A launch with no file to open then views the folder in Select, as
//! pressing it does (`browse.view`, which makes it the Develop folder), when the catalog still
//! lists it and nothing was chosen meanwhile; otherwise it starts as it would with nothing
//! remembered, saying nothing. From then on each change of the Develop folder is stored.
use super::{
    Editor,
    message::{Message, develop::DevelopMessage},
    select::Reread,
    tasks::{self, owner_task},
};
use crate::{
    mask_draft::{Brush, NEUTRAL_BRUSH},
    state::{preferences::PreferenceChange, select as model},
};
use iced::Task;
use luxforge_core::{
    ClientId, ClientSession, OwnerHandle, WorkspaceState,
    catalog_types::{CatalogFolderId, CatalogFolders, CatalogInfo, ViewSource},
    preferences::{BrushPreference, DevelopFolderPreference, ExportLast, WorkspacePreference},
};
use serde_json::json;
use std::path::{Path, PathBuf};

/// What a launch found of the remembered Develop folder: this catalog's identity, and the folder
/// when it is this catalog's and the catalog still lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RememberedFolder {
    pub(crate) catalog_id: String,
    pub(crate) folder: Option<CatalogFolderId>,
}

/// The remembered Develop folder as this launch follows it.
#[derive(Debug, Default)]
pub(crate) struct FolderMemory {
    /// The launch opened no file, so it views the remembered folder again once it is found.
    pub(crate) restore: bool,
    /// This catalog's identity, once read. Until then no change of the Develop folder is stored.
    pub(crate) catalog_id: Option<String>,
    /// The Develop folder as last compared with the preference, so each change is stored once.
    pub(crate) considered: Option<CatalogFolderId>,
}

/// `catalog.info`, then `folder.list` when `remembered` names this catalog: the body of the
/// launch's owner task, which a test runs as the task would.
pub(crate) fn remembered_folder_now(
    owner: &OwnerHandle,
    client: ClientId,
    remembered: Option<&DevelopFolderPreference>,
) -> Result<RememberedFolder, String> {
    fn parse<T: serde::de::DeserializeOwned>(value: serde_json::Value) -> Result<T, String> {
        serde_json::from_value(value).map_err(|error| error.to_string())
    }
    let (info, _) = tasks::call(owner, client, "catalog.info", json!({}))?;
    let catalog_id = parse::<CatalogInfo>(info)?.catalog_id;
    let folder = match remembered {
        Some(remembered) if remembered.catalog_id == catalog_id => {
            let (folders, _) = tasks::call(owner, client, "folder.list", json!({}))?;
            parse::<CatalogFolders>(folders)?
                .folders
                .iter()
                .any(|folder| folder.id == remembered.folder_id)
                .then(|| remembered.folder_id.clone())
        }
        _ => None,
    };
    Ok(RememberedFolder { catalog_id, folder })
}

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

    /// An export the person started from the sheet, the menu or the palette succeeded: store its
    /// settings and the preset it was, while unchanged, as the last export, which the sheet opens
    /// on next and Export again repeats. `None` (an evidence file) stores nothing.
    pub(crate) fn remember_export_last(&mut self, last: Option<ExportLast>) -> Task<Message> {
        let Some(last) = last else {
            return Task::none();
        };
        match self.preferences.applied() {
            Some(preferences) if preferences.export_last.as_deref() != Some(&last) => self
                .store_preferences(PreferenceChange {
                    export_last: Some(Some(Box::new(last))),
                    ..PreferenceChange::default()
                }),
            _ => Task::none(),
        }
    }
}

impl Editor {
    /// The launch's read of this catalog's identity and the remembered Develop folder, off the
    /// update loop.
    pub(super) fn read_remembered_folder(&self) -> Task<Message> {
        let remembered = self.preferences.applied_develop_folder().cloned();
        let (owner, client) = (self.owner.clone(), self.client);
        owner_task(
            move || remembered_folder_now(&owner, client, remembered.as_ref()),
            |result| Message::Develop(DevelopMessage::Remembered(result)),
        )
    }

    /// The launch's read answered. A launch with no file to open views the remembered folder in
    /// Select, as pressing it does, when the catalog still lists it and neither a view nor a
    /// Develop folder was chosen meanwhile; the status line keeps what the launch said. A folder
    /// gone, another catalog's or a failed read leaves the launch as it is, the stored folder
    /// kept.
    pub(crate) fn develop_folder_remembered(
        &mut self,
        result: Result<RememberedFolder, String>,
    ) -> Task<Message> {
        let restore = std::mem::take(&mut self.develop.remembered.restore);
        let found = match result {
            Ok(found) => found,
            Err(reason) => {
                self.event("develop_folder_unread", || json!({"reason": reason}));
                return Task::none();
            }
        };
        self.develop.remembered.catalog_id = Some(found.catalog_id);
        let Some(folder) = found.folder else {
            return Task::none();
        };
        if !restore || self.select.state.query.is_some() || self.develop_folder().is_some() {
            return Task::none();
        }
        self.event("develop_folder_restored", || json!({"folder": folder}));
        let task = self.evaluate(model::source_query(ViewSource::CatalogFolder {
            folder_id: folder,
            subfolders: true,
        }));
        self.select.reread = Reread::Quiet;
        task
    }

    /// After every message: once this catalog's identity is known, a Develop folder that changed
    /// since it was last compared is stored when the preference does not already hold it, and a
    /// Develop folder this catalog's preference named that is now gone stores `null`. A preference
    /// for another catalog is left alone while this one holds no folder; nothing is stored while the
    /// preferences are unreadable.
    pub(super) fn remember_develop_folder_preference(&mut self) -> Task<Message> {
        let Some(catalog_id) = self.develop.remembered.catalog_id.clone() else {
            return Task::none();
        };
        let now = self.select.state.catalog.develop_folder.as_ref();
        if self.develop.remembered.considered.as_ref() == now {
            return Task::none();
        }
        self.develop.remembered.considered = now.cloned();
        if self.preferences.stored().is_none() {
            return Task::none();
        }
        let held = self.preferences.applied_develop_folder();
        let change = match now {
            Some(folder)
                if held.is_none_or(|held| {
                    held.catalog_id != catalog_id || held.folder_id != *folder
                }) =>
            {
                Some(DevelopFolderPreference {
                    catalog_id,
                    folder_id: folder.clone(),
                })
            }
            None if held.is_some_and(|held| held.catalog_id == catalog_id) => None,
            _ => return Task::none(),
        };
        self.store_preferences(PreferenceChange {
            develop_folder: Some(change),
            ..PreferenceChange::default()
        })
    }
}

/// The folder a chosen destination is in, which the last export's settings name.
pub(crate) fn export_folder(destination: &Path) -> Option<PathBuf> {
    destination
        .parent()
        .filter(|folder| !folder.as_os_str().is_empty())
        .map(Path::to_path_buf)
}
