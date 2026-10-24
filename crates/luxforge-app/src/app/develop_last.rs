//! Picking up where the person left Develop ([develop from
//! folders](../../../../docs/design/develop-from-folders.md#develops-folder)).
//!
//! The Develop folder and the photograph last open in Develop are remembered as the `develop_last`
//! preference, with the identity of the catalog holding them (`catalog.info`'s `catalog_id`).
//! Every launch reads that identity in one owner task ([`remembered_place_now`]), and, when the
//! preference is this catalog's, where Develop can open again:
//!
//! - **The folder still listed.** It is read for the filmstrip, through a client of its own as `D`
//!   reads it, with the remembered photograph opened when it is still in it and the folder's first
//!   otherwise, the rule `D` follows. Select views the folder too, as pressing it does.
//! - **The folder gone, the photograph not.** The photograph is found in All photographs, on a
//!   client of its own (`browse.view`, `browse.select` of it, `browse.rows` of its row), and opened
//!   with its own folder in the filmstrip, as a double-click in a catalog view opens it.
//! - **Neither.** Nothing: the launch stays as it would be with nothing remembered.
//!
//! Only a launch with no file to open lands there, and only while nothing was viewed, chosen or
//! opened meanwhile, so a launch never opens twice. From then on each change of the Develop folder
//! or of the photograph open in Develop is stored through the one preference writer.
use super::{
    Editor,
    develop::folder_strip_now,
    message::{Message, develop::DevelopMessage},
    select::Reread,
    tasks::{call, owner_task},
};
use crate::state::{develop::FolderRead, preferences::PreferenceChange, select as model};
use iced::Task;
use luxforge_core::{
    AssetId, ClientId, ClientSession, OwnerHandle,
    catalog_types::{
        CatalogFolderId, CatalogFolders, CatalogInfo, RowItem, ViewQuery, ViewRows, ViewSource,
        ViewSummary,
    },
    preferences::DevelopLastPreference,
};
use serde_json::{Value, json};

/// Where a launch can open Develop again.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Landing {
    /// The remembered folder, still listed, read for the filmstrip, and the remembered photograph.
    Folder {
        read: FolderRead,
        asset: Option<AssetId>,
    },
    /// The remembered folder is gone and the photograph is not: its own folder, read for the
    /// filmstrip.
    Photograph { read: FolderRead, asset: AssetId },
}

/// What a launch found: this catalog's identity, and where Develop can open again, when the
/// preference is this catalog's and the catalog still holds its folder or photograph.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RememberedPlace {
    pub(crate) catalog_id: String,
    pub(crate) landing: Option<Landing>,
}

/// Where Develop was left, as this launch follows it.
#[derive(Debug, Default)]
pub(crate) struct DevelopMemory {
    /// The launch opened no file, so it lands where Develop was left once that is found.
    pub(crate) restore: bool,
    /// This catalog's identity, once read. Until then nothing is stored.
    pub(crate) catalog_id: Option<String>,
    /// The Develop folder as last compared with the preference.
    pub(crate) folder: Option<CatalogFolderId>,
    /// The photograph last open in Develop as last compared with the preference. It outlives the
    /// document: closing a photograph, or moving between two, does not forget it.
    pub(crate) asset: Option<AssetId>,
}

fn parse<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, String> {
    serde_json::from_value(value).map_err(|error| error.to_string())
}

/// `catalog.info`, then, when `remembered` is this catalog's, the folder it names read for the
/// filmstrip while `folder.list` lists it, or else the folder its photograph is in: the body of the
/// launch's owner task, which a test runs as the task would.
pub(crate) fn remembered_place_now(
    owner: &OwnerHandle,
    client: ClientId,
    remembered: Option<&DevelopLastPreference>,
) -> Result<RememberedPlace, String> {
    let (info, _) = call(owner, client, "catalog.info", json!({}))?;
    let catalog_id = parse::<CatalogInfo>(info)?.catalog_id;
    let Some(remembered) = remembered.filter(|remembered| remembered.catalog_id == catalog_id)
    else {
        return Ok(RememberedPlace {
            catalog_id,
            landing: None,
        });
    };
    let listed = match &remembered.folder_id {
        Some(folder) => {
            let (folders, _) = call(owner, client, "folder.list", json!({}))?;
            parse::<CatalogFolders>(folders)?
                .folders
                .iter()
                .any(|held| held.id == *folder)
                .then(|| folder.clone())
        }
        None => None,
    };
    let landing = match (listed, &remembered.asset_id) {
        (Some(folder), asset) => Some(Landing::Folder {
            read: folder_strip_now(owner, &folder)?,
            asset: asset.clone(),
        }),
        (None, Some(asset)) => match photo_folder_now(owner, asset)? {
            Some(folder) => Some(Landing::Photograph {
                read: folder_strip_now(owner, &folder)?,
                asset: asset.clone(),
            }),
            None => None,
        },
        (None, None) => None,
    };
    Ok(RememberedPlace {
        catalog_id,
        landing,
    })
}

/// The catalog folder `asset` is in, found in All photographs on a client of its own, so the
/// Select view's session is left as it is; `None` when the view does not hold it, as for a removed
/// or unknown photograph.
fn photo_folder_now(
    owner: &OwnerHandle,
    asset: &AssetId,
) -> Result<Option<CatalogFolderId>, String> {
    let client = owner.register();
    let result = (|| {
        let query = ViewQuery::of(ViewSource::AllPhotographs);
        let (summary, _) = call(owner, client, "browse.view", model::view_params(&query))?;
        let summary: ViewSummary = parse(summary)?;
        // A photograph the view does not hold is refused by name: not found.
        let Ok((session, _)) = call(
            owner,
            client,
            "browse.select",
            json!({"items": [{"asset_id": asset}]}),
        ) else {
            return Ok(None);
        };
        let session: ClientSession = parse(session)?;
        let Some(position) = session.browse.selection.ranges.first().map(|at| at.start) else {
            return Ok(None);
        };
        let (rows, _) = call(
            owner,
            client,
            "browse.rows",
            json!({"from": position, "count": 1, "revision": summary.revision}),
        )?;
        Ok(parse::<ViewRows>(rows)?
            .rows
            .into_iter()
            .find(|row| matches!(&row.item, RowItem::Photo { asset_id } if asset_id == asset))
            .and_then(|row| row.folder_id))
    })();
    owner.disconnect(client);
    result
}

impl Editor {
    /// The launch's read of this catalog's identity and where Develop was left, off the update
    /// loop.
    pub(super) fn read_remembered_place(&self) -> Task<Message> {
        let remembered = self.preferences.applied_develop_last().cloned();
        let (owner, client) = (self.owner.clone(), self.client);
        owner_task(
            move || remembered_place_now(&owner, client, remembered.as_ref()),
            |result| Message::Develop(DevelopMessage::Remembered(result)),
        )
    }

    /// The launch's read answered. A launch with no file to open, over which nothing was viewed,
    /// chosen or opened meanwhile, lands where Develop was left: the folder is the Develop folder
    /// and its filmstrip, and Develop opens on the remembered photograph when the strip holds it and
    /// on the strip's first otherwise. A remembered folder is viewed in Select as well, as pressing
    /// it does. An empty folder is held, opening nothing. Nothing found, or a failed read, leaves
    /// the launch as it is, saying nothing, and the preference kept.
    pub(crate) fn develop_place_remembered(
        &mut self,
        result: Result<RememberedPlace, String>,
    ) -> Task<Message> {
        let restore = std::mem::take(&mut self.develop.remembered.restore);
        let found = match result {
            Ok(found) => found,
            Err(reason) => {
                self.event("develop_last_unread", || json!({"reason": reason}));
                return Task::none();
            }
        };
        let held_asset = self
            .preferences
            .applied_develop_last()
            .filter(|held| held.catalog_id == found.catalog_id)
            .and_then(|held| held.asset_id.clone());
        self.develop.remembered.catalog_id = Some(found.catalog_id);
        let untouched = self.select.state.query.is_none()
            && self.develop_folder().is_none()
            && self.document.state.is_none()
            && self.develop.switch.is_none()
            && self.develop.file_open.is_none();
        let landing = match found.landing {
            Some(landing) if restore && untouched => landing,
            // What the launch or the person chose is stored as it changes from nothing.
            _ => return Task::none(),
        };
        let (read, asset, viewed) = match landing {
            Landing::Folder { read, asset } => {
                let source = ViewSource::CatalogFolder {
                    folder_id: read.folder.clone(),
                    subfolders: true,
                };
                let viewed = self.evaluate(model::source_query(source));
                self.select.reread = Reread::Quiet;
                (read, asset, viewed)
            }
            Landing::Photograph { read, asset } => (read, Some(asset), Task::none()),
        };
        self.event(
            "develop_last_restored",
            || json!({"folder": read.folder, "asset": asset, "photos": read.photos.len()}),
        );
        // What is stored already describes where Develop lands, so landing writes nothing back.
        self.develop.remembered.folder = Some(read.folder.clone());
        self.develop.remembered.asset = held_asset;
        let empty = read.photos.is_empty();
        self.choose_develop_folder(Some(read.folder.clone()));
        self.develop.strip_serial += 1;
        self.adopt_folder(read);
        if empty {
            return viewed;
        }
        let shown = self.show_strip_on(asset.as_ref());
        Task::batch([viewed, shown])
    }

    /// After every message: once this catalog's identity is known, a change of the Develop folder
    /// or of the photograph open in Develop since they were last compared is stored, when the
    /// preference does not already hold them; neither held stores `null` over this catalog's
    /// preference. A preference for another catalog is left alone until something is chosen here;
    /// nothing is stored while the preferences are unreadable.
    pub(super) fn remember_develop_last(&mut self) -> Task<Message> {
        let memory = &self.develop.remembered;
        let Some(catalog_id) = &memory.catalog_id else {
            return Task::none();
        };
        let folder = self.select.state.catalog.develop_folder.as_ref();
        let asset = self
            .document
            .state
            .as_ref()
            .map(|state| &state.asset.id)
            .or(memory.asset.as_ref());
        if memory.folder.as_ref() == folder && memory.asset.as_ref() == asset {
            return Task::none();
        }
        let catalog_id = catalog_id.clone();
        let (folder, asset) = (folder.cloned(), asset.cloned());
        self.develop.remembered.folder = folder.clone();
        self.develop.remembered.asset = asset.clone();
        if self.preferences.stored().is_none() {
            return Task::none();
        }
        let held = self.preferences.applied_develop_last();
        let change = if folder.is_none() && asset.is_none() {
            if held.is_none_or(|held| held.catalog_id != catalog_id) {
                return Task::none();
            }
            None
        } else {
            let now = DevelopLastPreference {
                catalog_id,
                folder_id: folder,
                asset_id: asset,
            };
            if held == Some(&now) {
                return Task::none();
            }
            Some(now)
        };
        self.store_preferences(PreferenceChange {
            develop_last: Some(change),
            ..PreferenceChange::default()
        })
    }
}
