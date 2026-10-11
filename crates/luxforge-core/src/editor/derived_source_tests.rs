//! Derived sources through the editor service: publication as one owner transaction after durable
//! worker objects, failure at every boundary, the retention closure across removal, shared chunks,
//! index deletion, reopening and collection, owner requests that read no payload, and explicit
//! refusals that keep stored data as it is.
use super::derived_source::{attach_source, link_asset_source, record_source};
use super::*;
use crate::artifacts::{
    ArtifactMeta, ArtifactWriter, Collected, LiveArtifacts, collect_files,
    derived_source::{
        DerivedSourceWriter, PublishedSource, SOURCES, Step, fail, payload_work,
        read_derived_source, source_object_path,
    },
};
use crate::catalog_types::{CatalogFolderId, FileAvailability, HeaderMetadata, VolumeId};
use crate::{EntryId, HistoryEntry, Snapshot};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(1);

/// A directory of its own for one test, so the catalog file and its artifact root sit side by side.
fn directory(name: &str) -> PathBuf {
    let directory = std::env::temp_dir().join(format!(
        "luxforge-derived-sources-{}-{}-{name}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&directory).unwrap();
    directory.canonicalize().unwrap()
}

fn jpeg() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/s0/orientation-1.jpg")
}

/// A catalog holding one imported JPEG, whose catalog folder and volume derived photographs share.
fn catalog(directory: &Path) -> (PathBuf, EditorService, AssetId) {
    let catalog = directory.join("catalog.sqlite");
    let mut service = EditorService::open(&catalog).unwrap();
    let jpeg = service.import(&jpeg()).unwrap().asset.id;
    (catalog, service, jpeg)
}

/// Planes with signed values and headroom, different for each `seed`, except that every source's
/// first chunk holds the same values when `shared_first` is set, so two sources share that chunk.
fn planes(width: u32, height: u32, seed: u32, shared_first: bool) -> Vec<f32> {
    let plane = (width * height) as usize;
    let mut planes = vec![0.0; 3 * plane];
    for channel in 0..3 {
        for y in 0..height {
            for x in 0..width {
                let seed = if shared_first && x < 1024 && y < 1024 {
                    0
                } else {
                    seed
                };
                let mix = (x * 31 + y * 17 + channel as u32 * 7919 + seed * 104_729) % 1000;
                planes[channel * plane + (y * width + x) as usize] = (mix as f32 - 300.0) / 97.0;
            }
        }
    }
    planes
}

fn never() -> AtomicBool {
    AtomicBool::new(false)
}

fn write(writer: &DerivedSourceWriter, width: u32, height: u32, values: &[f32]) -> PublishedSource {
    writer
        .write_planes(width, height, values, None, &never())
        .unwrap()
}

/// The catalog folder and volume of the catalog's first photograph.
fn place(service: &EditorService) -> (CatalogFolderId, VolumeId) {
    service
        .connection
        .query_row(
            "SELECT catalog_folder_id, volume_id FROM assets ORDER BY row_id LIMIT 1",
            [],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .map(|(folder, volume)| {
            (
                CatalogFolderId::parse(folder).unwrap(),
                VolumeId::parse(volume).unwrap(),
            )
        })
        .unwrap()
}

/// The rows a merge publishes for a derived photograph, as later publication writes them: the
/// asset row of a derived source, its capture row, its Original and its state, then its source
/// references, all in the caller's transaction. `extra` runs last in the same transaction.
fn publish_rows(
    service: &mut EditorService,
    published: &PublishedSource,
    extra: impl FnOnce(&rusqlite::Transaction<'_>) -> Result<(), Error>,
) -> Result<AssetId, Error> {
    let (folder, volume) = place(service);
    let asset_id = AssetId::new();
    let identity = format!("derived:{asset_id}");
    let record = AssetRecord {
        id: asset_id.clone(),
        source_root: PathBuf::new(),
        locator: PathBuf::from(&identity),
        fingerprint: published.id.to_string(),
        file_identity: identity.clone(),
        byte_len: published.payload_bytes(),
        width: published.width,
        height: published.height,
        source: SourceKind::Derived {
            interpretation: published.interpretation(),
        },
    };
    let original = HistoryEntry {
        id: EntryId::new(),
        asset_id: asset_id.clone(),
        sequence: 0,
        action_id: "original".into(),
        label: "Original".into(),
        parameters: json!({}),
        actor: "system".into(),
        timestamp_ms: now_ms(),
        request_id: None,
        base_revision: 0,
        result_revision: 0,
        snapshot: Snapshot::original(asset_id.clone()),
        undo_parent: None,
        restore_target: None,
    };
    let root = service.artifact_root().to_path_buf();
    super::write(&mut service.connection, |tx| {
        insert_photograph(
            tx,
            &root,
            &original,
            &NewAsset {
                record: &record,
                canonical_locator: &identity,
                catalog_folder_id: &folder,
                source_folder: Path::new("derived"),
                volume_id: &volume,
                file_name: "merge.hdr",
                developed_ms: now_ms(),
                removed_ms: None,
                availability: FileAvailability::Available,
                checked_ms: now_ms(),
                develop_moment: None,
            },
            &HeaderMetadata::default(),
            None,
        )?;
        link_asset_source(tx, &root, &asset_id, published)?;
        extra(tx)
    })?;
    Ok(asset_id)
}

fn publish(service: &mut EditorService, published: &PublishedSource) -> AssetId {
    publish_rows(service, published, |_| Ok(())).unwrap()
}

fn count(service: &EditorService, table: &str) -> i64 {
    service
        .connection
        .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .unwrap()
}

/// Every object in `sources/`.
fn stored(service: &EditorService) -> BTreeSet<String> {
    match fs::read_dir(service.artifact_root().join(SOURCES)) {
        Err(_) => BTreeSet::new(),
        Ok(entries) => entries
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect(),
    }
}

/// The objects the catalog must keep, worked out apart from the reference tables: each photograph's
/// stored interpretation names its source, whose manifest, read from disk here, names its chunks.
fn independent_closure(service: &EditorService) -> BTreeSet<String> {
    let mut statement = service
        .connection
        .prepare("SELECT source_json FROM assets")
        .unwrap();
    let interpretations: Vec<String> = statement
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let mut closure = BTreeSet::new();
    for json in interpretations {
        let kind: SourceKind = serde_json::from_str(&json).unwrap();
        let Some(interpretation) = kind.derived() else {
            continue;
        };
        let manifest = interpretation.source_id.as_str()["source-".len()..].to_owned();
        let text = fs::read(source_object_path(service.artifact_root(), &manifest)).unwrap();
        let value: Value = serde_json::from_slice(&text).unwrap();
        for chunk in value["chunks"].as_array().unwrap() {
            closure.insert(chunk["sha256"].as_str().unwrap().to_owned());
        }
        closure.insert(manifest);
    }
    closure
}

fn collect(service: &mut EditorService) -> Collected {
    let collection = service.plan_collection().unwrap();
    collect_files(&collection, &never()).unwrap()
}

/// Read the photograph's source as a worker would, through what the owner hands it.
fn read_back(service: &EditorService, asset: &AssetId) -> Vec<f32> {
    let read = service.derived_source_read(asset).unwrap();
    read_derived_source(&read, &never())
        .unwrap()
        .shared_planes()
        .0
        .to_vec()
}

fn same_bits(read: &[f32], written: &[f32]) -> bool {
    read.len() == written.len()
        && read
            .iter()
            .zip(written)
            .all(|(read, written)| read.to_bits() == written.to_bits())
}

/// Every boundary of a publication — each worker write step and the owner's transaction — either
/// publishes a complete source or leaves nothing visible, and collection keeps exactly the closure
/// an independent walk of the photographs finds, plus what this process published while open.
#[test]
fn a_publication_is_complete_or_invisible_at_every_boundary() {
    let directory = directory("boundaries");
    let (catalog, mut service, _) = catalog(&directory);
    let (width, height) = (1100, 1030);
    let kept_values = planes(width, height, 1, false);
    let kept_source = write(
        &service.derived_source_writer(),
        width,
        height,
        &kept_values,
    );
    let kept = publish(&mut service, &kept_source);
    let visible = |service: &EditorService| {
        (
            count(service, "assets"),
            count(service, "derived_sources"),
            count(service, "derived_source_chunks"),
            count(service, "asset_sources"),
        )
    };
    let before = visible(&service);
    assert_eq!(before, (2, 1, 4, 1));
    // The worker's boundaries: the publication fails and the catalog is never asked.
    for (seed, step) in [
        Step::StageChunk(0),
        Step::RenameChunk(1),
        Step::StageChunk(3),
        Step::StageManifest,
        Step::RenameManifest,
        Step::SyncDirectory,
    ]
    .into_iter()
    .enumerate()
    {
        fail::at(step);
        let values = planes(width, height, 10 + seed as u32, false);
        let error = service
            .derived_source_writer()
            .write_planes(width, height, &values, None, &never())
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::FileAccess, "{step:?}: {error}");
        assert_eq!(visible(&service), before, "{step:?}");
    }
    // The owner's transaction: refused at the source reference, after the asset row and the
    // source rows were written in it, and refused after everything was written.
    let failed_values = planes(width, height, 30, false);
    let failed = write(
        &service.derived_source_writer(),
        width,
        height,
        &failed_values,
    );
    service
        .connection
        .execute_batch(
            "CREATE TRIGGER injected_failure BEFORE INSERT ON asset_sources BEGIN
                 SELECT RAISE(ABORT, 'injected failure');
             END;",
        )
        .unwrap();
    let error = publish_rows(&mut service, &failed, |_| Ok(())).unwrap_err();
    assert!(error.detail.contains("injected failure"), "{error}");
    assert_eq!(visible(&service), before);
    service
        .connection
        .execute_batch("DROP TRIGGER injected_failure")
        .unwrap();
    let error = publish_rows(&mut service, &failed, |_| {
        Err(Error::internal("injected failure at the end"))
    })
    .unwrap_err();
    assert!(error.detail.contains("at the end"), "{error}");
    assert_eq!(visible(&service), before);
    // Everything this process published stays while it is open, recorded or not.
    let published = stored(&service);
    assert!(published.is_superset(&independent_closure(&service)));
    let collected = collect(&mut service);
    assert_eq!((collected.source_rows, collected.source_objects), (0, 0));
    assert_eq!(stored(&service), published);
    // Reopened, nothing is live: only the closure of the photographs is kept.
    drop(service);
    let mut service = EditorService::open(&catalog).unwrap();
    let collected = collect(&mut service);
    assert!(collected.source_objects > 0);
    assert_eq!(stored(&service), independent_closure(&service));
    assert_eq!(visible(&service), before);
    assert!(same_bits(&read_back(&service, &kept), &kept_values));
    // A retry after the failures publishes the same source, now complete and visible.
    let retried = write(
        &service.derived_source_writer(),
        width,
        height,
        &failed_values,
    );
    assert_eq!(retried, failed);
    let asset = publish(&mut service, &retried);
    assert!(same_bits(&read_back(&service, &asset), &failed_values));
    drop(service);
    let mut service = EditorService::open(&catalog).unwrap();
    collect(&mut service);
    assert_eq!(stored(&service), independent_closure(&service));
    assert!(same_bits(&read_back(&service, &asset), &failed_values));
    drop(service);
    fs::remove_dir_all(directory).unwrap();
}

/// A source is retained for every photograph that holds it, removed or not, through deleting the
/// index, reopening and collecting, while unrelated artifacts are collected; emptying Removed
/// releases a source only when no remaining photograph holds it, and never a chunk another source
/// shares.
#[test]
fn a_source_is_retained_until_emptying_removed_releases_its_last_photograph() {
    let directory = directory("retention");
    let (catalog, mut service, _) = catalog(&directory);
    let (width, height) = (2048, 4);
    let a_values = planes(width, height, 1, true);
    let b_values = planes(width, height, 2, true);
    let writer = service.derived_source_writer();
    let a_source = write(&writer, width, height, &a_values);
    let b_source = write(&writer, width, height, &b_values);
    assert_eq!(
        a_source.chunks[0], b_source.chunks[0],
        "the sources share their first chunk"
    );
    assert_ne!(a_source.chunks[1], b_source.chunks[1]);
    let a = publish(&mut service, &a_source);
    let b = publish(&mut service, &b_source);
    let b_again = publish(&mut service, &b_source);
    // Both photographs of B removed, the second later; A stays visible.
    for (asset, removed_ms) in [(&b, 1_000), (&b_again, 2_000)] {
        service
            .connection
            .execute(
                "UPDATE assets SET removed_ms = ?1 WHERE id = ?2",
                rusqlite::params![removed_ms, asset.as_str()],
            )
            .unwrap();
    }
    // An artifact an earlier session published and nothing records.
    let orphan = ArtifactWriter::new(
        service.artifact_root().to_path_buf(),
        service.catalog_id().to_owned(),
        LiveArtifacts::default(),
    )
    .write(
        b"an unrelated module artifact",
        ArtifactMeta {
            kind: "tint".into(),
            width: None,
            height: None,
            colour: None,
        },
        "test.writer",
    )
    .unwrap()
    .0;
    // The index is made, then deleted with the catalog closed.
    drop(service.index().unwrap());
    let index = service.index_dir().to_path_buf();
    assert!(index.exists());
    drop(service);
    fs::remove_dir_all(&index).unwrap();
    let mut service = EditorService::open(&catalog).unwrap();
    let closure = independent_closure(&service);
    assert_eq!(
        closure.len(),
        2 + 2 + 1,
        "two manifests, one shared and two own chunks"
    );
    let collected = collect(&mut service);
    assert_eq!(collected.objects, 1, "the unrelated artifact");
    assert!(!crate::artifacts::object_path(service.artifact_root(), &orphan.id).exists());
    assert_eq!((collected.source_rows, collected.source_objects), (0, 0));
    assert_eq!(stored(&service), closure);
    for (asset, values) in [(&a, &a_values), (&b, &b_values), (&b_again, &b_values)] {
        assert!(same_bits(&read_back(&service, asset), values));
    }
    // Emptying the earlier-removed photograph of B: B is still held by the other.
    let (emptied, collection) = service.empty_removed(1).unwrap();
    assert_eq!(emptied.assets, std::slice::from_ref(&b));
    assert!(emptied.sources.is_empty());
    assert!(collection.is_none());
    assert_eq!(stored(&service), closure);
    assert!(same_bits(&read_back(&service, &b_again), &b_values));
    // Emptying the last: B's manifest and own chunk go, the shared chunk stays with A.
    let (emptied, collection) = service.empty_removed(10).unwrap();
    assert_eq!(emptied.assets, std::slice::from_ref(&b_again));
    assert_eq!(emptied.sources, std::slice::from_ref(&b_source.id));
    let collected = collect_files(&collection.unwrap(), &never()).unwrap();
    assert_eq!((collected.source_rows, collected.source_objects), (1, 2));
    assert_eq!(stored(&service), independent_closure(&service));
    assert!(stored(&service).contains(&a_source.chunks[0].sha256));
    assert_eq!(
        (
            count(&service, "derived_sources"),
            count(&service, "asset_sources")
        ),
        (1, 1)
    );
    assert!(same_bits(&read_back(&service, &a), &a_values));
    drop(service);
    fs::remove_dir_all(directory).unwrap();
}

/// The owner's part of a publication, a read plan and a collection plan stat files and write rows;
/// none reads or hashes a chunk or manifest. The worker's read hashes each.
#[test]
fn owner_requests_read_and_hash_no_payload() {
    let directory = directory("owner");
    let (_, mut service, _) = catalog(&directory);
    let values = planes(1100, 8, 1, false);
    let published = write(&service.derived_source_writer(), 1100, 8, &values);
    assert_eq!(
        payload_work::take(),
        3,
        "the writer hashed two chunks and the manifest"
    );
    let asset = publish(&mut service, &published);
    let read = service.derived_source_read(&asset).unwrap();
    service.plan_collection().unwrap();
    service.empty_removed(10).unwrap();
    service.state(&asset).unwrap();
    assert_eq!(payload_work::take(), 0, "the owner read and hashed nothing");
    read_derived_source(&read, &never()).unwrap();
    assert_eq!(
        payload_work::take(),
        3,
        "the worker verified the manifest and each chunk"
    );
    drop(service);
    fs::remove_dir_all(directory).unwrap();
}

/// A link names the photograph's own source or is refused, a source is recorded only whole and
/// present, and the reference rows can be neither changed nor deleted outside emptying Removed.
#[test]
fn linking_refuses_any_source_but_the_photographs_own_and_keeps_the_rows() {
    let directory = directory("links");
    let (_, mut service, jpeg) = catalog(&directory);
    let writer = service.derived_source_writer();
    let first = write(&writer, 4, 4, &planes(4, 4, 1, false));
    let second = write(&writer, 4, 4, &planes(4, 4, 2, false));
    let asset = publish(&mut service, &first);
    let root = service.artifact_root().to_path_buf();
    let attempt = |service: &mut EditorService,
                   f: &dyn Fn(&rusqlite::Transaction<'_>) -> Result<(), Error>| {
        super::write(&mut service.connection, |tx| f(tx))
    };
    let rows = |service: &EditorService| {
        (
            count(service, "derived_sources"),
            count(service, "derived_source_chunks"),
            count(service, "asset_sources"),
        )
    };
    let before = rows(&service);
    // Linking the same source again changes nothing.
    service.link_derived_source(&asset, &first).unwrap();
    assert_eq!(rows(&service), before);
    let error = attempt(&mut service, &|tx| {
        link_asset_source(tx, &root, &asset, &second)
    })
    .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Validation);
    assert!(error.detail.contains("is derived from"), "{error}");
    let error = attempt(&mut service, &|tx| attach_source(tx, &jpeg, &first.id)).unwrap_err();
    assert!(
        error.detail.contains("is not a derived photograph"),
        "{error}"
    );
    // A source is recorded only with every object present at its recorded length.
    fs::remove_file(source_object_path(&root, &second.chunks[0].sha256)).unwrap();
    let error = attempt(&mut service, &|tx| record_source(tx, &root, &second)).unwrap_err();
    assert_eq!(error.kind, ErrorKind::SourceUnavailable);
    assert!(error.detail.contains("is missing"), "{error}");
    let mut short = first.clone();
    short.chunks.pop();
    let error = attempt(&mut service, &|tx| record_source(tx, &root, &short)).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Validation);
    let error = publish_rows(&mut service, &second, |_| Ok(())).unwrap_err();
    assert_eq!(error.kind, ErrorKind::SourceUnavailable);
    assert_eq!(rows(&service), before, "nothing was recorded");
    for (statement, refusal) in [
        (
            "UPDATE asset_sources SET source_id = source_id",
            "keeps its derived source",
        ),
        ("DELETE FROM asset_sources", "references are permanent"),
        (
            "UPDATE derived_sources SET width = width",
            "derived sources are immutable",
        ),
        (
            "UPDATE derived_source_chunks SET bytes = bytes",
            "derived sources are immutable",
        ),
    ] {
        let error = service.connection.execute(statement, []).unwrap_err();
        assert!(error.to_string().contains(refusal), "{statement}: {error}");
    }
    // A source a photograph holds cannot be deleted under it.
    assert!(
        service
            .connection
            .execute("DELETE FROM derived_sources", [])
            .is_err()
    );
    assert_eq!(rows(&service), before);
    drop(service);
    fs::remove_dir_all(directory).unwrap();
}

/// A derived photograph is its own kind: its history reads, its interpretation serializes as one,
/// and the paths that cannot read it yet — preparation, camera previews — refuse it by name rather
/// than reading it as a JPEG.
#[test]
fn a_derived_photograph_reads_its_history_and_preparation_refuses_it_by_name() {
    let directory = directory("kind");
    let (_, mut service, _) = catalog(&directory);
    let published = write(
        &service.derived_source_writer(),
        8,
        2,
        &planes(8, 2, 1, false),
    );
    let asset = publish(&mut service, &published);
    let state = service.state(&asset).unwrap();
    assert_eq!(state.asset.source.tag(), SourceTag::Derived);
    assert!(state.asset.source.white_balance_available());
    let reported = serde_json::to_value(&state.asset.source).unwrap();
    assert_eq!(
        reported,
        json!({
            "kind": "derived",
            "interpretation": {
                "source_id": published.id.as_str(),
                "width": 8,
                "height": 2,
                "colour": "linear-srgb-d65",
            }
        })
    );
    let stored_kind: String = service
        .connection
        .query_row(
            "SELECT source_kind FROM assets WHERE id = ?1",
            [asset.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(stored_kind, "derived");
    let error = FilePreparation::for_recipe(&state.asset, &state.current_entry.snapshot.recipe)
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::UnsupportedInput);
    assert!(error.detail.contains("derived source"), "{error}");
    let error = service
        .verified_prepared(&state.asset, &state.current_entry.snapshot.recipe)
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::UnsupportedInput);
    drop(service);
    fs::remove_dir_all(directory).unwrap();
}

/// A missing or corrupt object, or a root that is another catalog's, fails the read by name and
/// leaves everything stored as it is; history stays readable and collection keeps the damaged
/// source's objects.
#[test]
fn a_damaged_source_fails_its_reads_by_name_and_is_kept() {
    let directory = directory("damaged");
    let (_, mut service, _) = catalog(&directory);
    let values = planes(1030, 3, 1, false);
    let published = write(&service.derived_source_writer(), 1030, 3, &values);
    let asset = publish(&mut service, &published);
    let root = service.artifact_root().to_path_buf();
    let chunk = source_object_path(&root, &published.chunks[1].sha256);
    let original = fs::read(&chunk).unwrap();
    fs::remove_file(&chunk).unwrap();
    let error = service.derived_source_read(&asset).unwrap_err();
    assert_eq!(error.kind, ErrorKind::SourceUnavailable);
    assert_eq!(
        error.detail,
        format!("derived source {} is missing", published.id)
    );
    service.state(&asset).unwrap();
    // Corrupt at its own length: the owner's stat passes, the worker's hash names it.
    let mut damaged = original.clone();
    damaged[5] ^= 0x40;
    fs::write(&chunk, &damaged).unwrap();
    let read = service.derived_source_read(&asset).unwrap();
    let error = read_derived_source(&read, &never()).unwrap_err();
    assert_eq!(error.kind, ErrorKind::SourceUnavailable);
    assert!(error.detail.contains("chunk 1 is corrupt"), "{error}");
    let closure = independent_closure(&service);
    collect(&mut service);
    assert_eq!(
        stored(&service),
        closure,
        "a damaged source is still retained"
    );
    assert_eq!(fs::read(&chunk).unwrap(), damaged, "and left as it is");
    service.state(&asset).unwrap();
    fs::write(&chunk, &original).unwrap();
    assert!(same_bits(&read_back(&service, &asset), &values));
    // A root whose manifest names another catalog is refused and kept.
    let manifest = root.join(crate::artifacts::MANIFEST);
    let text = fs::read_to_string(&manifest).unwrap();
    let foreign = text.replace(service.catalog_id(), "another-catalog");
    fs::write(&manifest, &foreign).unwrap();
    service.clear_prepared_artifacts();
    let error = service.derived_source_read(&asset).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Incompatible);
    assert!(error.detail.contains("another-catalog"), "{error}");
    let error = service
        .plan_collection()
        .map(|collection| collect_files(&collection, &never()).unwrap_err());
    assert_eq!(error.unwrap().kind, ErrorKind::Incompatible);
    assert_eq!(fs::read_to_string(&manifest).unwrap(), foreign);
    assert_eq!(stored(&service), closure);
    drop(service);
    fs::remove_dir_all(directory).unwrap();
}
