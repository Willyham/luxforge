//! Sending developed photographs back (`asset.send-back`, P9): a photograph that holds nothing
//! but its Original entry — no history beyond it, no named version, no collection — leaves the
//! catalog, its record deleted, and its file is picked again, as one library change. One with more
//! is refused, saying why; it leaves the catalog only by being removed. A Develop's undo sends its
//! photographs back the same way, through the `developed-asset` item
//! (`crate::library::items`), and re-picks their files from what the Develop recorded.
use crate::{
    AssetId, Error,
    catalog_types::{AssetRowId, FileSignature, LibraryChangeRow, LibraryItem, Pick, Volume},
    editor::library_rows,
    index::volume_of,
    library::journal::{Desired, Request},
};
use rusqlite::Connection;
use serde_json::json;
use std::path::Path;

/// A send-back, planned: each photograph's `developed-asset` gone absent and its file's pick, and
/// the volumes those picks are on.
pub(crate) struct SendBack {
    pub changes: Vec<(LibraryItem, Desired)>,
    pub volumes: Vec<Volume>,
}

/// Plan sending `assets` back, checking each before anything is written: a photograph with more
/// than its Original is refused (`conflict`, saying why and naming the item in `data.items`), and
/// so is one whose original is not at its locator as the file it was developed from, since its file
/// could not be picked again. Each file is picked again with its signature now, unless it is
/// picked already. On the owner: three counts and one stat a photograph.
pub(crate) fn plan(
    catalog: &Connection,
    assets: &[(AssetId, AssetRowId)],
    request: Request<'_>,
    now_ms: i64,
) -> Result<SendBack, Error> {
    let mut changes = Vec::with_capacity(2 * assets.len());
    let mut volumes: Vec<Volume> = Vec::new();
    for (asset, _) in assets {
        let item = LibraryItem::DevelopedAsset {
            asset_id: asset.clone(),
        };
        let source = library_rows::asset_source(catalog, asset)?
            .ok_or_else(|| Error::validation(format!("unknown asset {asset}")))?;
        let name = file_name(&source.locator);
        if let Some(why) = library_rows::kept_by(catalog, asset)? {
            return Err(refused(
                &item,
                format!("{name} {why}: it leaves the catalog only by being removed"),
            ));
        }
        let metadata = source
            .locator
            .metadata()
            .ok()
            .filter(|metadata| metadata.is_file());
        let signature = metadata.as_ref().map(FileSignature::of);
        let here = metadata.as_ref().is_some_and(|metadata| {
            crate::editor::source_signature(&source.locator, metadata).file_identity()
                == source.file_identity
        });
        let (Some(signature), true) = (signature, here) else {
            return Err(refused(
                &item,
                format!(
                    "{name} cannot be sent back: its original is not at {}, so it could not be \
                     picked again; relink it first, or remove it",
                    source.locator.display()
                ),
            ));
        };
        let volume = volume_of(&source.locator, now_ms)?;
        if !volumes.iter().any(|known| known.id == volume.id) {
            volumes.push(volume.clone());
        }
        let pick = Pick {
            path: source.locator.clone(),
            signature,
            volume_id: volume.id,
            actor: request.actor.to_owned(),
            request_id: request.request_id.to_owned(),
            picked_ms: now_ms,
            file_id: None,
        };
        let pick = serde_json::to_value(&pick)
            .map_err(|error| Error::internal(format!("cannot encode a pick: {error}")))?;
        changes.push((item, Desired::Value(None)));
        changes.push((
            LibraryItem::Pick {
                path: source.locator,
            },
            Desired::UnlessPresent(pick),
        ));
    }
    Ok(SendBack { changes, volumes })
}

/// A send-back's label: "Sent back DSC_0412.NEF", "Sent back 5 photographs".
pub(crate) fn label(rows: &[LibraryChangeRow]) -> String {
    let sent: Vec<&LibraryChangeRow> = rows
        .iter()
        .filter(|row| matches!(row.item, LibraryItem::DevelopedAsset { .. }))
        .collect();
    match sent.as_slice() {
        [one] => format!(
            "Sent back {}",
            one.before
                .as_ref()
                .and_then(|before| before.get("path"))
                .and_then(|path| path.as_str())
                .map_or_else(|| one.item.key(), |path| file_name(Path::new(path)))
        ),
        many => format!("Sent back {} photographs", many.len()),
    }
}

fn refused(item: &LibraryItem, message: String) -> Error {
    Error::conflict(message).with_data(json!({"items": [item], "count": 1}))
}

/// A path's file name as a person reads it.
fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}
