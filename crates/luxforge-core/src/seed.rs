//! Generated catalogs and indexes, written in bulk, for `cargo xtask generate-catalog` and tests.
//!
//! A seeded catalog is an ordinary format-13 catalog: every photograph has its asset row, its
//! Original entry, its state row and its capture row, written through the same row writers the
//! import, the index lane and the library use (`editor/catalog_rows.rs`, [`crate::index`]), so the
//! core opens, lists and reads it exactly as it would a catalog developed by hand. Its originals
//! need not exist: a seeded photograph whose file is absent reads as missing or offline, as its row
//! says. Nothing here is on any request path; the product never seeds.
//!
//! Everything a seeded row names is the caller's, identities included, so the same rows give the
//! same catalog: an Original's entry, snapshot and layer identities are derived from the asset's.
//! An Original's stack is built by the rule a Develop's is ([`editor::original_recipe`]), over the
//! linked modules and the default preferences, so a seeded RAW photograph starts from the layers a
//! developed one does.
use crate::{
    AssetId, AssetRecord, EditorService, EntryId, Error, HistoryEntry, LayerId, ModuleRegistry,
    OriginalPreferences, Snapshot, SnapshotId, SourceKind,
    catalog_types::{
        AssetRowId, CatalogFolder, CatalogFolderId, Collection, CollectionId, FileAvailability,
        FileId, FileRecord, HeaderMetadata, IndexRoot, IndexedFolder, MomentId, Pick, Volume,
        VolumeId,
    },
    editor::{self, NewAsset, RawInterpretation},
    index::{self, IndexDb, IndexOpened},
};
use rusqlite::{Connection, params};
use serde_json::json;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

/// What a seeded photograph's original is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SeedKind {
    /// A JPEG: an Original with an empty recipe.
    Jpeg,
    /// A RAW: an Original holding its source-development layer, over a synthetic interpretation of
    /// the photograph's size. Its original never exists, so nothing ever prepares it; it is there
    /// so kinds can be told apart in views, facets and filters.
    Raw,
}

/// One seeded photograph: its asset row, its catalog columns and its capture metadata.
#[derive(Clone, Debug)]
pub struct SeedAsset {
    pub id: AssetId,
    pub kind: SeedKind,
    /// Its original's absolute path; its folder is the photograph's source folder.
    pub locator: PathBuf,
    /// Its original's SHA-256, as 64 lowercase hex digits.
    pub fingerprint: String,
    /// Its original's file identity as the catalog stores it (`unix:<device>:<inode>`), unique.
    pub file_identity: String,
    pub byte_len: u64,
    pub width: u32,
    pub height: u32,
    pub catalog_folder_id: CatalogFolderId,
    pub volume_id: VolumeId,
    pub developed_ms: i64,
    pub removed_ms: Option<i64>,
    pub availability: FileAvailability,
    pub checked_ms: i64,
    pub develop_moment: Option<MomentId>,
    pub header: HeaderMetadata,
    pub place: Option<String>,
}

/// Writes one new catalog in bulk: each call is one transaction.
pub struct CatalogSeeder {
    connection: Connection,
    artifact_root: PathBuf,
    /// The linked modules, which contribute to each Original as they do to a Develop's.
    registry: Arc<ModuleRegistry>,
}

impl CatalogSeeder {
    /// A new, empty format-13 catalog at `path`, identified as `catalog_id`. Refused when anything
    /// is already there.
    pub fn create(path: &Path, catalog_id: &str) -> Result<Self, Error> {
        if path.exists() {
            return Err(Error::validation(format!(
                "{} already exists; a catalog is seeded only into a new path",
                path.display()
            )));
        }
        let mut connection = Connection::open(path)?;
        // A seeded catalog is thrown away on failure, so it is written without waiting for the
        // drive; the owner opens it with its own durable settings afterwards.
        connection.execute_batch(
            "PRAGMA foreign_keys=ON; PRAGMA synchronous=OFF; PRAGMA journal_mode=MEMORY;
             PRAGMA cache_size=-262144;",
        )?;
        EditorService::create_schema(&mut connection, catalog_id)?;
        Ok(Self {
            connection,
            artifact_root: editor::default_artifact_root(path),
            registry: Arc::new(ModuleRegistry::builtin()),
        })
    }

    /// Build Originals over `registry`'s modules instead of the linked ones, for a test of a
    /// module's contribution.
    #[cfg(test)]
    pub(crate) fn serving(self, registry: Arc<ModuleRegistry>) -> Self {
        Self { registry, ..self }
    }

    pub fn volumes(&mut self, volumes: &[Volume]) -> Result<(), Error> {
        editor::write(&mut self.connection, |tx| {
            volumes
                .iter()
                .try_for_each(|volume| editor::upsert_volume(tx, volume))
        })
    }

    /// Catalog folders, each after its parent.
    pub fn folders(&mut self, folders: &[CatalogFolder]) -> Result<(), Error> {
        editor::write(&mut self.connection, |tx| {
            folders
                .iter()
                .try_for_each(|folder| editor::insert_catalog_folder(tx, folder))
        })
    }

    /// Photographs, each with its Original entry, state row and capture row, answering their rows
    /// in order. Their folders and volumes must already be seeded.
    pub fn assets(&mut self, assets: &[SeedAsset]) -> Result<Vec<AssetRowId>, Error> {
        let artifact_root = &self.artifact_root;
        let registry = &*self.registry;
        editor::write(&mut self.connection, |tx| {
            let mut rows = Vec::with_capacity(assets.len());
            for asset in assets {
                let (record, entry) = original(registry, asset)?;
                let canonical = asset.locator.to_string_lossy();
                let source_folder = asset.locator.parent().unwrap_or(Path::new(""));
                let file_name = asset
                    .locator
                    .file_name()
                    .map(|name| name.to_string_lossy())
                    .unwrap_or_default();
                let row = editor::insert_asset(
                    tx,
                    &NewAsset {
                        record: &record,
                        canonical_locator: &canonical,
                        catalog_folder_id: &asset.catalog_folder_id,
                        source_folder,
                        volume_id: &asset.volume_id,
                        file_name: &file_name,
                        developed_ms: asset.developed_ms,
                        removed_ms: asset.removed_ms,
                        availability: asset.availability,
                        checked_ms: asset.checked_ms,
                        develop_moment: asset.develop_moment.as_ref(),
                    },
                )?;
                editor::insert_entry(tx, artifact_root, &entry)?;
                tx.execute(
                    "INSERT INTO asset_state (asset_id, current_entry_id, revision, redo_json)
                     VALUES (?1, ?2, 0, '[]')",
                    params![asset.id.as_str(), entry.id.as_str()],
                )?;
                editor::insert_capture(tx, row, &asset.header, asset.place.as_deref())?;
                rows.push(row);
            }
            Ok(rows)
        })
    }

    /// Collections, smart collections and groups, each after its parent.
    pub fn collections(&mut self, collections: &[Collection]) -> Result<(), Error> {
        editor::write(&mut self.connection, |tx| {
            collections
                .iter()
                .try_for_each(|collection| editor::insert_collection(tx, collection))
        })
    }

    /// Photographs in plain collections: `(collection, photograph, added_ms)`.
    pub fn members(&mut self, members: &[(CollectionId, AssetRowId, i64)]) -> Result<(), Error> {
        editor::write(&mut self.connection, |tx| {
            members.iter().try_for_each(|(collection, row, added_ms)| {
                editor::insert_member(tx, collection, *row, *added_ms)
            })
        })
    }

    pub fn picks(&mut self, picks: &[Pick]) -> Result<(), Error> {
        editor::write(&mut self.connection, |tx| {
            picks
                .iter()
                .try_for_each(|pick| editor::insert_pick(tx, pick))
        })
    }

    pub fn indexed_folders(&mut self, folders: &[IndexedFolder]) -> Result<(), Error> {
        editor::write(&mut self.connection, |tx| {
            folders
                .iter()
                .try_for_each(|folder| editor::insert_indexed_folder(tx, folder))
        })
    }

    /// Finish: gather the query planner's statistics and close the catalog.
    pub fn finish(self) -> Result<(), Error> {
        self.connection.execute_batch("ANALYZE;")?;
        self.connection.close().map_err(|(_, error)| error.into())
    }
}

/// A seeded photograph's asset record and Original entry, with identities derived from its own:
/// its first layer is `layer-<suffix>`, each one after it `layer-<suffix>-<n>`.
fn original(
    registry: &ModuleRegistry,
    asset: &SeedAsset,
) -> Result<(AssetRecord, HistoryEntry), Error> {
    let suffix = &asset.id.as_str()[AssetId::PREFIX.len()..];
    let source = match asset.kind {
        SeedKind::Jpeg => SourceKind::Jpeg,
        SeedKind::Raw => SourceKind::Raw {
            metadata: synthetic_interpretation(asset)?,
        },
    };
    let mut layers = 0;
    let recipe = editor::original_recipe(
        registry,
        &source,
        &asset.header,
        OriginalPreferences::default(),
        || {
            layers += 1;
            LayerId::parse(match layers {
                1 => format!("{}{suffix}", LayerId::PREFIX),
                n => format!("{}{suffix}-{n}", LayerId::PREFIX),
            })
        },
    )?;
    let record = AssetRecord {
        id: asset.id.clone(),
        source_root: asset
            .locator
            .parent()
            .unwrap_or(Path::new(""))
            .to_path_buf(),
        locator: asset.locator.clone(),
        fingerprint: asset.fingerprint.clone(),
        file_identity: asset.file_identity.clone(),
        byte_len: asset.byte_len,
        width: asset.width,
        height: asset.height,
        source,
    };
    let entry = HistoryEntry {
        id: EntryId::parse(format!("{}{suffix}", EntryId::PREFIX))?,
        asset_id: asset.id.clone(),
        sequence: 0,
        action_id: "original".into(),
        label: "Original".into(),
        parameters: json!({}),
        actor: "system".into(),
        timestamp_ms: asset.developed_ms,
        request_id: None,
        base_revision: 0,
        result_revision: 0,
        snapshot: Snapshot {
            id: SnapshotId::parse(format!("{}{suffix}", SnapshotId::PREFIX))?,
            asset_id: asset.id.clone(),
            recipe,
        },
        undo_parent: None,
        restore_target: None,
    };
    Ok((record, entry))
}

/// A RAW interpretation of the photograph's size under a pinned mode, naming its camera.
fn synthetic_interpretation(asset: &SeedAsset) -> Result<RawInterpretation, Error> {
    use luxforge_raw::{RawLayout, RawMetadata, RawMode, RawRect};
    let rect = RawRect {
        x: 0,
        y: 0,
        width: asset.width,
        height: asset.height,
    };
    let camera = asset.header.camera.as_ref();
    RawInterpretation::new(RawMetadata {
        make: camera.map_or("Generated", |body| body.make.as_str()).into(),
        model: camera.map_or("RAW", |body| body.model.as_str()).into(),
        mode: RawMode::from_id("NikonZ6Lossless14").ok_or_else(|| {
            Error::internal("the pinned seeding mode is not in the camera catalog")
        })?,
        layout: RawLayout::Mosaic,
        sensor_width: asset.width,
        sensor_height: asset.height,
        active_area: rect,
        default_crop: rect,
        cfa_width: 2,
        cfa_height: 2,
        cfa: vec![0, 1, 1, 2],
        black_cfa: vec![0, 0, 0, 0],
        black_base: 1008.0,
        black_channels: [0.0; 4],
        black_repeat_width: 1,
        black_repeat_height: 1,
        black_repeat: vec![0.0],
        sensor_white: 15520.0,
        as_shot_gains: [2.0, 1.0, 1.5],
        libraw_flip: 0,
        rgb_cam: [
            [1.6, -0.5, -0.1, 0.0],
            [-0.2, 1.5, -0.3, 0.0],
            [0.0, -0.4, 1.4, 0.0],
        ],
        cam_xyz: [
            [0.9, -0.3, -0.1],
            [-0.4, 1.2, 0.2],
            [-0.1, 0.2, 0.6],
            [0.0, 0.0, 0.0],
        ],
        backend: "generated".into(),
        exif_orientation: 1,
        libraw_inset: Some(rect),
        format_identity: "generated".into(),
        warnings: vec![],
        dng_corrections: None,
    })
}

/// Writes one new index database in bulk: each call is one transaction.
pub struct IndexSeeder {
    index: IndexDb,
}

impl IndexSeeder {
    /// A new, empty index for the catalog at `catalog` (identified as `catalog_id`), at the path the
    /// core looks for it ([`index::index_dir`]). Refused when an index is already there.
    pub fn create(catalog: &Path, catalog_id: &str) -> Result<Self, Error> {
        let dir = index::index_dir(catalog);
        if dir.join(index::INDEX_FILE).exists() {
            return Err(Error::validation(format!(
                "{} already holds an index",
                dir.display()
            )));
        }
        let (index, opened) = IndexDb::open(&dir, catalog_id)?;
        if opened != IndexOpened::Created {
            return Err(Error::internal("a new index was not created"));
        }
        index
            .connection()
            .execute_batch("PRAGMA synchronous=OFF;")?;
        Ok(Self { index })
    }

    pub fn roots(&mut self, roots: &[IndexRoot]) -> Result<(), Error> {
        let tx = self.index.connection_mut().transaction()?;
        for root in roots {
            index::upsert_root(&tx, root)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Files, answering their rows in order.
    pub fn files(&mut self, files: &[FileRecord]) -> Result<Vec<FileId>, Error> {
        let tx = self.index.connection_mut().transaction()?;
        let ids = files
            .iter()
            .map(|file| index::upsert_file(&tx, file))
            .collect::<Result<Vec<_>, _>>()?;
        tx.commit()?;
        Ok(ids)
    }

    /// Finish: gather the query planner's statistics, fold the write-ahead log into the database
    /// and close it.
    pub fn finish(self) -> Result<(), Error> {
        let connection = self.index.connection();
        connection.execute_batch("ANALYZE;")?;
        connection.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        SourceTag,
        catalog_types::{
            CameraBody, CaptureTime, CollectionKind, EventSpan, FileSignature, GeoPosition,
            HeaderState, RootKind,
        },
    };
    use luxforge_testbase::paths::temp_dir;

    fn volume() -> Volume {
        Volume {
            id: VolumeId::parse("volume-generated-ssd").unwrap(),
            mount_point: "/Volumes/Photos SSD".into(),
            label: "Photos SSD".into(),
            removable: false,
            platform_id: None,
            last_seen_ms: 1,
        }
    }

    fn header(second: u32) -> HeaderMetadata {
        HeaderMetadata {
            capture: CaptureTime::from_exif(
                &format!("2026:09:12 10:15:{second:02}"),
                Some("250"),
                Some("+02:00"),
            ),
            position: GeoPosition::new(47.66, 9.175, None),
            camera: Some(CameraBody {
                make: "LEICA CAMERA AG".into(),
                model: "LEICA Q3".into(),
                serial: Some("5412345".into()),
            }),
            lens: Some("SUMMILUX 1:1.7/28 ASPH.".into()),
            ..HeaderMetadata::default()
        }
    }

    fn asset(index: u32, kind: SeedKind, folder: &CatalogFolderId) -> SeedAsset {
        SeedAsset {
            id: AssetId::parse(format!("asset-{index:032x}")).unwrap(),
            kind,
            locator: format!("/Volumes/Photos SSD/2026/Konstanz/L100{index:04}.DNG").into(),
            fingerprint: format!("{index:064x}"),
            file_identity: format!("unix:7:{index}"),
            byte_len: 40_000_000,
            width: 6000,
            height: 4000,
            catalog_folder_id: folder.clone(),
            volume_id: volume().id,
            developed_ms: 1_800_000_000_000 + i64::from(index),
            removed_ms: None,
            availability: FileAvailability::Offline,
            checked_ms: 5,
            develop_moment: None,
            header: header(index),
            place: Some("Konstanz".into()),
        }
    }

    /// A seeded catalog opens as any other: its photographs list, their state reads with an
    /// admitted Original of each kind, their capture rows read back, and its index opens beside it.
    #[test]
    fn a_seeded_catalog_and_index_open_with_the_core() {
        let dir = temp_dir("seed");
        let catalog = dir.join("catalog.sqlite");
        let mut seeder = CatalogSeeder::create(&catalog, "catalog-generated").unwrap();
        assert!(
            CatalogSeeder::create(&catalog, "again").is_err(),
            "a new path only"
        );
        seeder.volumes(&[volume()]).unwrap();
        let folder = CatalogFolder {
            id: CatalogFolderId::parse("folder-konstanz-2026").unwrap(),
            name: "Konstanz · Sep 2026".into(),
            parent_id: None,
            created_ms: 1,
            event: Some(EventSpan {
                start_ms: 1,
                end_ms: 2,
                event_id: None,
            }),
            count: 0,
            year: None,
        };
        seeder.folders(std::slice::from_ref(&folder)).unwrap();
        let assets = [
            asset(1, SeedKind::Jpeg, &folder.id),
            asset(2, SeedKind::Raw, &folder.id),
        ];
        let rows = seeder.assets(&assets).unwrap();
        let collection = Collection {
            id: CollectionId::parse("collection-portfolio").unwrap(),
            name: "Portfolio".into(),
            parent_id: None,
            kind: CollectionKind::Collection,
            query: None,
            created_ms: 3,
            count: None,
        };
        seeder
            .collections(std::slice::from_ref(&collection))
            .unwrap();
        seeder
            .members(&[(collection.id.clone(), rows[1], 4)])
            .unwrap();
        seeder.finish().unwrap();

        let service = EditorService::open(&catalog).unwrap();
        let listed = service.asset_ids(10).unwrap();
        assert_eq!(listed.len(), 2);
        assert_eq!(
            service.state(&listed[1]).unwrap().asset.source.tag(),
            SourceTag::Raw
        );
        for asset in &assets {
            let state = service.state(&asset.id).unwrap();
            assert_eq!(state.current_entry.label, "Original");
            service
                .describe_entry(&asset.id, None)
                .expect("the Original's stack is admitted for its kind");
        }
        let (read, place) = editor::capture_of(&service.connection, rows[0])
            .unwrap()
            .unwrap();
        assert_eq!(read, assets[0].header);
        assert_eq!(place.as_deref(), Some("Konstanz"));
        drop(service);

        let mut index = IndexSeeder::create(&catalog, "catalog-generated").unwrap();
        index
            .roots(&[IndexRoot {
                path: "/Volumes/Photos SSD".into(),
                kind: RootKind::Indexed,
                volume_id: volume().id,
                listed_ms: Some(9),
                file_count: Some(1),
                offline: true,
            }])
            .unwrap();
        let file = FileRecord {
            path: assets[1].locator.clone(),
            folder: assets[1].locator.parent().unwrap().to_path_buf(),
            name: "L1000002.DNG".into(),
            volume_id: volume().id,
            signature: FileSignature {
                len: 40_000_000,
                modified_ns: 8,
                identity: None,
            },
            kind: SourceTag::Raw,
            header: HeaderState::Ok(Box::new(header(2))),
            last_seen_ms: 9,
        };
        let ids = index.files(std::slice::from_ref(&file)).unwrap();
        index.finish().unwrap();
        assert!(IndexSeeder::create(&catalog, "catalog-generated").is_err());
        let (opened, state) =
            IndexDb::open(&index::index_dir(&catalog), "catalog-generated").unwrap();
        assert_eq!(state, IndexOpened::Opened);
        assert_eq!(
            index::file(opened.connection(), ids[0]).unwrap(),
            Some(file)
        );
        drop(opened);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
