//! A merge's lifecycle against a real catalog: publication with its provenance and result-led
//! stack across index deletion and reopening, retries, creation undo after editing the result and
//! redo with the inputs offline, Remove and Put back, one active stack per input, another actor's
//! change, exact-match reuse, the recheck before publication, Empty Removed and an input
//! photograph's own removal. Every before/after value is read independently from the catalog's
//! rows, and every original file's hash is checked unchanged.
use super::*;
use crate::{
    ErrorKind,
    artifacts::derived_source::{payload_work, read_derived_source},
    catalog_types::{
        AssetRowId, Collection, CollectionId, CollectionKind, HdrDevelopment, HdrParameters,
        HdrResolution, MERGE_PROVENANCE_FORMAT, MembershipValue, MergeInput, MergeParameters,
        MergeProvider, MergeResolution,
    },
    editor::{distinct_jpeg, insert_collection, mutation},
    index::volume_of,
    library::{journal, remove},
    merge::hdr::{
        AlignmentEvidence, AlignmentMode, Coverage, DeghostMode, ExposureOrigin, ExposureSettings,
        FrameMetadata, InputAlignment, MetadataSource, MotionDiagnostics, PixelRect, Recording,
        ReferenceOrigin, Registration, ResolvedExposure, ResolvedExposures, RigidTransform,
        SensorLayout,
    },
};
use luxforge_testbase::paths::{jpeg, temp_dir};
use sha2::{Digest, Sha256};
use std::{fs, sync::atomic::AtomicBool};

const WIDTH: u32 = 48;
const HEIGHT: u32 = 32;

pub(super) fn never() -> AtomicBool {
    AtomicBool::new(false)
}

/// A catalog beside a bracket of `frames` distinct files, the second developed into the catalog
/// as an ordinary photograph, so the merge has both a plain file input and a catalog photograph
/// input.
pub(super) struct Bench {
    pub(super) directory: PathBuf,
    pub(super) catalog: PathBuf,
    pub(super) service: EditorService,
    pub(super) files: Vec<PathBuf>,
    pub(super) developed: AssetId,
}

impl Bench {
    pub(super) fn new(name: &str, frames: usize) -> Self {
        let directory = temp_dir(&format!("hdr-merge-lifecycle-{name}"))
            .canonicalize()
            .unwrap();
        let files: Vec<PathBuf> = (1..=frames)
            .map(|frame| {
                distinct_jpeg(
                    &jpeg(),
                    &directory
                        .join("bracket")
                        .join(format!("DSC_{frame:04}.JPG")),
                )
            })
            .collect();
        let catalog = directory.join("catalog.sqlite");
        let mut service = EditorService::open(&catalog).unwrap();
        let developed = service.import(&files[1]).unwrap().asset.id;
        Self {
            directory,
            catalog,
            service,
            files,
            developed,
        }
    }

    pub(super) fn reopen(&mut self) {
        let placeholder = EditorService::open(&self.directory.join("placeholder.sqlite")).unwrap();
        drop(std::mem::replace(&mut self.service, placeholder));
        self.service = EditorService::open(&self.catalog).unwrap();
    }

    /// The provenance of a merge of `inputs` (positions in the bracket), with `deghost` as its
    /// one varied parameter, the middle input the reference.
    pub(super) fn provenance(&self, inputs: &[usize], deghost: DeghostMode) -> MergeProvenance {
        let count = inputs.len();
        let reference = count / 2;
        let merge_inputs = inputs
            .iter()
            .map(|&index| {
                let path = self.files[index].clone();
                let signature = FileSignature::of(&path.metadata().unwrap());
                MergeInput {
                    volume_id: volume_of(&path, 1).unwrap().id,
                    signature,
                    asset_id: (index == 1).then(|| self.developed.clone()),
                    frame: FrameMetadata {
                        identity: path.to_string_lossy().into_owned(),
                        make: Some("NIKON CORPORATION".into()),
                        model: Some("NIKON Z 6".into()),
                        recording: Recording::Raw {
                            mode: "nikon-z6-14bit-lossless".into(),
                            layout: SensorLayout::Bayer,
                        },
                        width: WIDTH,
                        height: HEIGHT,
                        orientation: 1,
                        exposure: ExposureSettings {
                            shutter_s: Some(1.0 / 2f64.powi(6 - index as i32)),
                            f_number: Some(8.0),
                            iso: Some(100),
                            bias_ev: Some(index as f64 - 1.0),
                            focal_mm: Some(24.0),
                        },
                        captured_at_ms: Some(1_000 + index as i64),
                    },
                    path,
                }
            })
            .collect();
        let offsets: Vec<f64> = (0..count).map(|at| at as f64 - reference as f64).collect();
        MergeProvenance {
            format: MERGE_PROVENANCE_FORMAT,
            kind: MergeKind::Hdr,
            provider: MergeProvider {
                module: "luxforge.hdr".into(),
                version: 1,
            },
            inputs: merge_inputs,
            parameters: MergeParameters::Hdr(HdrParameters {
                alignment: crate::merge::hdr::AlignmentOptions {
                    alignment: AlignmentMode::Auto,
                    deghost,
                },
                ..HdrParameters::default()
            }),
            resolved: MergeResolution::Hdr(HdrResolution {
                exposures: ResolvedExposures {
                    reference,
                    reference_origin: ReferenceOrigin::ZeroBias,
                    inputs: offsets
                        .iter()
                        .enumerate()
                        .map(|(at, &offset_ev)| ResolvedExposure {
                            offset_ev,
                            scale: 2f64.powf(-offset_ev),
                            origin: if at == reference {
                                ExposureOrigin::Reference
                            } else {
                                ExposureOrigin::Metadata {
                                    source: MetadataSource::Settings,
                                }
                            },
                        })
                        .collect(),
                },
                development: HdrDevelopment {
                    white_balance: [2.0, 1.0, 1.5],
                    cam_xyz: [
                        [0.7, -0.1, 0.0],
                        [-0.4, 1.2, 0.2],
                        [0.0, 0.1, 0.6],
                        [0.0; 3],
                    ],
                },
                registration: Registration {
                    mode: AlignmentMode::Auto,
                    reference,
                    width: WIDTH,
                    height: HEIGHT,
                    inputs: (0..count)
                        .map(|at| InputAlignment {
                            transform: RigidTransform::identity(WIDTH, HEIGHT),
                            evidence: if at == reference {
                                AlignmentEvidence::Reference
                            } else {
                                AlignmentEvidence::Fitted {
                                    via: None,
                                    correlation: 0.93,
                                    samples: 1500,
                                    inlier_fraction: 0.8,
                                    residual_ev: 0.04,
                                    brightness_ev: offsets[at],
                                    uncertainty_px: 0.05,
                                    confidence: 0.9,
                                    snapped_px: Some(0.01),
                                }
                            },
                        })
                        .collect(),
                },
                coverage: Coverage {
                    frame_width: WIDTH,
                    frame_height: HEIGHT,
                    rect: PixelRect {
                        x: 0,
                        y: 0,
                        width: WIDTH,
                        height: HEIGHT,
                    },
                    pixels_lost: 0,
                },
                motion: MotionDiagnostics::default(),
                fusion: crate::merge::hdr::FusionDiagnostics {
                    pixels: u64::from(WIDTH * HEIGHT),
                    highlight_extended: 40,
                    ..Default::default()
                },
            }),
        }
    }

    /// A durable derived source of signed values with headroom, different for each `seed`.
    pub(super) fn source(&self, seed: u32) -> PublishedSource {
        let plane = (WIDTH * HEIGHT) as usize;
        let values: Vec<f32> = (0..3 * plane)
            .map(|at| ((at as u32 * 37 + seed * 101) % 500) as f32 / 97.0 - 1.5)
            .collect();
        self.service
            .derived_source_writer()
            .write_planes(WIDTH, HEIGHT, &values, None, &never())
            .unwrap()
    }

    pub(super) fn folder(&self) -> CatalogFolderId {
        let folder: String = self
            .service
            .connection
            .query_row(
                "SELECT catalog_folder_id FROM assets WHERE id = ?1",
                [self.developed.as_str()],
                |row| row.get(0),
            )
            .unwrap();
        CatalogFolderId::parse(folder).unwrap()
    }

    /// Publish a merge of `provenance` with `source` as `actor` under `request_id`.
    pub(super) fn publish_as(
        &mut self,
        actor: &str,
        request_id: &str,
        provenance: &MergeProvenance,
        source: &PublishedSource,
        extra: impl FnOnce(
            &Transaction<'_>,
            &MergeId,
            &AssetId,
        ) -> Result<Vec<(LibraryItem, Desired)>, Error>,
    ) -> Result<PublishedMerge, Error> {
        let folder = self.folder();
        let volume = volume_of(&provenance.reference_input().path, 1).unwrap();
        let name = format!(
            "{} HDR",
            provenance
                .reference_input()
                .path
                .file_stem()
                .unwrap()
                .to_string_lossy()
        );
        self.service.publish_merge(
            request("merge.create", actor, request_id),
            &Publication {
                provenance,
                source,
                catalog_folder_id: &folder,
                file_name: &name,
                header: &HeaderMetadata::default(),
                place: None,
                volumes: std::slice::from_ref(&volume),
                replaces: None,
            },
            extra,
        )
    }

    pub(super) fn publish(
        &mut self,
        request_id: &str,
        inputs: &[usize],
        seed: u32,
    ) -> PublishedMerge {
        let provenance = self.provenance(inputs, DeghostMode::Auto);
        let source = self.source(seed);
        self.publish_as("agent", request_id, &provenance, &source, |_, _, _| {
            Ok(Vec::new())
        })
        .unwrap()
    }

    pub(super) fn row(&self, asset: &AssetId) -> AssetRowId {
        AssetRowId(
            self.service
                .connection
                .query_row(
                    "SELECT row_id FROM assets WHERE id = ?1",
                    [asset.as_str()],
                    |row| row.get(0),
                )
                .unwrap(),
        )
    }

    pub(super) fn remove_as(
        &mut self,
        actor: &str,
        request_id: &str,
        asset: &AssetId,
    ) -> Result<Outcome, Error> {
        let assets = vec![(asset.clone(), self.row(asset))];
        self.service.library_write(|tx| {
            remove::remove(tx, assets, 5_000)?.apply(tx, request("asset.remove", actor, request_id))
        })
    }

    pub(super) fn restore_as(
        &mut self,
        actor: &str,
        request_id: &str,
        asset: &AssetId,
    ) -> Result<Outcome, Error> {
        let assets = vec![(asset.clone(), self.row(asset))];
        self.service.library_write(|tx| {
            remove::restore(tx, assets)?.apply(tx, request("asset.restore", actor, request_id))
        })
    }

    pub(super) fn undo_as(&mut self, actor: &str, request_id: &str) -> Result<Outcome, Error> {
        self.service
            .library_write(|tx| journal::undo(tx, request("library.undo", actor, request_id)))
    }

    pub(super) fn redo_as(&mut self, actor: &str, request_id: &str) -> Result<Outcome, Error> {
        self.service
            .library_write(|tx| journal::redo(tx, request("library.redo", actor, request_id)))
    }

    /// Each bracket file's SHA-256, read from disk.
    pub(super) fn hashes(&self) -> Vec<String> {
        self.files
            .iter()
            .map(|file| format!("{:x}", Sha256::digest(fs::read(file).unwrap())))
            .collect()
    }

    /// The photograph's removal time, read from its row: `None` while it is visible.
    pub(super) fn removed_ms(&self, asset: &AssetId) -> Option<i64> {
        self.service
            .connection
            .query_row(
                "SELECT removed_ms FROM assets WHERE id = ?1",
                [asset.as_str()],
                |row| row.get(0),
            )
            .unwrap()
    }

    /// The input paths each active stack claims, as `(path, merge)` in path order.
    pub(super) fn claimed(&self) -> Vec<(String, String)> {
        self.service
            .connection
            .prepare("SELECT path, merge_id FROM stack_claims ORDER BY path")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    pub(super) fn count(&self, table: &str) -> i64 {
        self.service
            .connection
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap()
    }

    /// The photograph's whole history as stored: each entry's identity and text, in order, and
    /// its head.
    pub(super) fn history(&self, asset: &AssetId) -> (Vec<(String, String)>, String) {
        let entries = self
            .service
            .connection
            .prepare("SELECT id, entry_json FROM entries WHERE asset_id = ?1 ORDER BY sequence")
            .unwrap()
            .query_map([asset.as_str()], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        let head = self
            .service
            .connection
            .query_row(
                "SELECT current_entry_id FROM asset_state WHERE asset_id = ?1",
                [asset.as_str()],
                |row| row.get(0),
            )
            .unwrap();
        (entries, head)
    }

    /// The source the photograph holds, by its asset-source reference.
    pub(super) fn held(&self, asset: &AssetId) -> Option<String> {
        self.service
            .connection
            .query_row(
                "SELECT source_id FROM asset_sources WHERE asset_id = ?1",
                [asset.as_str()],
                |row| row.get(0),
            )
            .optional()
            .unwrap()
    }

    pub(super) fn read_back(&self, asset: &AssetId) -> Vec<u32> {
        let read = self.service.derived_source_read(asset).unwrap();
        read_derived_source(&read, &never())
            .unwrap()
            .shared_planes()
            .0
            .iter()
            .map(|value| value.to_bits())
            .collect()
    }

    pub(super) fn finish(self) {
        let Self {
            directory, service, ..
        } = self;
        drop(service);
        fs::remove_dir_all(directory).unwrap();
    }
}

pub(super) fn request<'a>(method: &'a str, actor: &'a str, request_id: &'a str) -> Request<'a> {
    Request {
        method,
        actor,
        request_id,
    }
}

fn stack(merge: &MergeId) -> LibraryItem {
    LibraryItem::MergeStack {
        merge_id: merge.clone(),
    }
}

fn removal(asset: &AssetId) -> LibraryItem {
    LibraryItem::AssetRemoval {
        asset_id: asset.clone(),
    }
}

/// A publication is one library change making the result, its source, provenance and active stack
/// visible together; a retry answers it again; and every row survives deleting the index and
/// reopening the catalog, the stack still led by the same result over the same inputs.
#[test]
fn hdr_merge_lifecycle_publication_survives_index_deletion_and_reopening() {
    let mut bench = Bench::new("publication", 3);
    let hashes = bench.hashes();
    let provenance = bench.provenance(&[0, 1, 2], DeghostMode::Auto);
    let source = bench.source(1);
    let assets_before = bench.count("assets");
    let published = bench
        .publish_as("agent", "create-1", &provenance, &source, |_, _, _| {
            Ok(Vec::new())
        })
        .unwrap();
    let PublishedMerge {
        merge_id, result, ..
    } = &published;
    let change = published.outcome.answer().change.unwrap();
    assert!(!published.outcome.answer().deduplicated);
    let detail = journal::inspect(&bench.service.connection, change.0).unwrap();
    assert_eq!(detail.change.label, "Merged DSC_0002 HDR");
    let created_ms: i64 = bench
        .service
        .connection
        .query_row(
            "SELECT created_ms FROM merges WHERE id = ?1",
            [merge_id.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        detail.rows,
        [
            crate::catalog_types::LibraryChangeRow {
                item: removal(result),
                before: Some(json!({"removed_ms": created_ms})),
                after: None,
            },
            crate::catalog_types::LibraryChangeRow {
                item: stack(merge_id),
                before: Some(json!({"state": "undone"})),
                after: Some(json!({"state": "active"})),
            },
        ]
    );
    assert_eq!(bench.count("assets"), assets_before + 1);
    assert_eq!(bench.held(result).as_deref(), Some(source.id.as_str()));
    assert_eq!(bench.removed_ms(result), None);
    let stored: String = bench
        .service
        .connection
        .query_row(
            "SELECT source_json FROM assets WHERE id = ?1",
            [result.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        serde_json::from_str::<SourceKind>(&stored).unwrap(),
        SourceKind::Derived {
            interpretation: source.interpretation()
        }
    );
    // The stack: led by the result, its members the exact inputs in order with their offsets,
    // each claimed by this stack alone.
    let record = record(&bench.service.connection, merge_id).unwrap();
    assert_eq!(record.result_asset_id, *result);
    assert_eq!(record.source_id, source.id);
    assert_eq!(record.state, MergeState::Active);
    assert_eq!(record.provenance, provenance);
    let members: Vec<(PathBuf, Option<AssetId>, Option<f64>)> = record
        .members
        .iter()
        .map(|member| {
            (
                member.path.clone(),
                member.asset_id.clone(),
                member.offset_ev,
            )
        })
        .collect();
    assert_eq!(
        members,
        [
            (bench.files[0].clone(), None, Some(-1.0)),
            (
                bench.files[1].clone(),
                Some(bench.developed.clone()),
                Some(0.0)
            ),
            (bench.files[2].clone(), None, Some(1.0)),
        ]
    );
    let claimed: Vec<(String, String)> = bench
        .files
        .iter()
        .map(|file| (file.to_string_lossy().into_owned(), merge_id.to_string()))
        .collect();
    assert_eq!(bench.claimed(), claimed);
    // A retry answers the first attempt's merge and writes nothing.
    let another = bench.source(2);
    let retried = bench
        .publish_as("agent", "create-1", &provenance, &another, |_, _, _| {
            Ok(Vec::new())
        })
        .unwrap();
    assert_eq!((&retried.merge_id, &retried.result), (merge_id, result));
    assert!(retried.outcome.answer().deduplicated);
    assert_eq!(bench.count("assets"), assets_before + 1);
    assert_eq!(bench.count("merges"), 1);
    // The index is made, then deleted with the catalog closed; reopened, every row reads the same.
    drop(bench.service.index().unwrap());
    let index = bench.service.index_dir().to_path_buf();
    assert!(index.exists());
    bench.reopen();
    fs::remove_dir_all(&index).unwrap();
    bench.reopen();
    assert_eq!(
        super::record(&bench.service.connection, merge_id).unwrap(),
        record
    );
    assert_eq!(bench.claimed(), claimed);
    assert_eq!(
        merge_of(&bench.service.connection, result)
            .unwrap()
            .as_ref(),
        Some(merge_id)
    );
    let after = bench
        .publish_as("agent", "create-1", &provenance, &source, |_, _, _| {
            Ok(Vec::new())
        })
        .unwrap();
    assert_eq!(
        &after.result, result,
        "a retry after reopening answers it too"
    );
    assert_eq!(bench.hashes(), hashes);
    bench.finish();
}

/// Undoing a merge's creation after editing its result hides the result in Removed, labelled
/// undone, deactivates its stack and reverts only the membership the creation added, keeping the
/// result's source, history and edits; a Develop's send-back refuses it by name; and redo, after
/// reopening with every input offline, restores the same result without reading an input or
/// merging again.
#[test]
fn hdr_merge_lifecycle_creation_undo_after_an_edit_and_redo_with_the_inputs_offline() {
    let mut bench = Bench::new("undo", 3);
    let hashes = bench.hashes();
    // A collection with an unrelated member, which the creation adds the result to through its
    // extra items, as a development set's membership joins it.
    let collection = Collection {
        id: CollectionId::new(),
        name: "Portfolio".into(),
        parent_id: None,
        kind: CollectionKind::Collection,
        query: None,
        created_ms: 1,
        count: None,
    };
    let developed_row = bench.row(&bench.developed);
    crate::editor::write(&mut bench.service.connection, |tx| {
        insert_collection(tx, &collection)?;
        crate::editor::insert_member(tx, &collection.id, developed_row, 2)
    })
    .unwrap();
    let provenance = bench.provenance(&[0, 1, 2], DeghostMode::Auto);
    let source = bench.source(1);
    let collection_id = collection.id.clone();
    let published = bench
        .publish_as("agent", "create", &provenance, &source, |_, _, result| {
            Ok(vec![(
                LibraryItem::Membership {
                    collection_id,
                    asset_id: result.clone(),
                },
                Desired::Value(Some(value(MembershipValue { added_ms: 3 })?)),
            )])
        })
        .unwrap();
    let (merge, result) = (published.merge_id.clone(), published.result.clone());
    let members = |bench: &Bench| -> Vec<String> {
        bench
            .service
            .connection
            .prepare(
                "SELECT a.id FROM collection_members m JOIN assets a ON a.row_id = m.asset_row
                 WHERE m.collection_id = ?1 ORDER BY m.added_ms",
            )
            .unwrap()
            .query_map([collection.id.as_str()], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    assert_eq!(
        members(&bench),
        [bench.developed.to_string(), result.to_string()]
    );
    // An edit of the result.
    let revision = bench.service.state(&result).unwrap().revision;
    bench
        .service
        .apply_action(
            &result,
            mutation(revision, "contrast"),
            "set-basic",
            json!({"contrast": 20.0}),
        )
        .unwrap();
    let history = bench.history(&result);
    assert_eq!(history.0.len(), 2, "the Original and the edit");
    let pixels = bench.read_back(&result);
    payload_work::take();
    // A Develop's send-back never takes a merge's result out of the catalog.
    let error = crate::library::develop::send_back::plan(
        &bench.service.connection,
        &[(result.clone(), bench.row(&result))],
        request("asset.send-back", "agent", "send"),
        5_000,
    )
    .err()
    .unwrap();
    assert_eq!(error.kind, ErrorKind::Conflict);
    assert!(
        error.detail.contains("DSC_0002 HDR is a merged photograph"),
        "{error}"
    );
    // Undo: Removed, labelled undone, the stack inactive and only the creation's membership gone.
    let undone = bench.undo_as("agent", "undo").unwrap();
    assert_eq!(undone.answer().items, 3);
    let created_ms: i64 = bench
        .service
        .connection
        .query_row(
            "SELECT created_ms FROM merges WHERE id = ?1",
            [merge.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(bench.removed_ms(&result), Some(created_ms));
    assert_eq!(
        state(&bench.service.connection, &merge).unwrap(),
        Some(MergeState::Undone)
    );
    assert!(bench.claimed().is_empty());
    assert_eq!(members(&bench), [bench.developed.to_string()]);
    assert_eq!(bench.history(&result), history);
    assert_eq!(bench.held(&result).as_deref(), Some(source.id.as_str()));
    let matched = matching(&bench.service.connection, &provenance.match_key().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(
        (matched.result_asset_id, matched.state),
        (result.clone(), MergeState::Undone),
        "a matching request is offered Put back merge"
    );
    // Reopened with every input offline, redo restores the same result.
    bench.reopen();
    let offline = bench.directory.join("offline");
    fs::rename(bench.directory.join("bracket"), &offline).unwrap();
    payload_work::take();
    bench.redo_as("agent", "redo").unwrap();
    assert_eq!(payload_work::take(), 0, "nothing was read or merged again");
    assert_eq!(bench.removed_ms(&result), None);
    assert_eq!(
        state(&bench.service.connection, &merge).unwrap(),
        Some(MergeState::Active)
    );
    assert_eq!(bench.claimed().len(), 3);
    assert_eq!(
        members(&bench),
        [bench.developed.to_string(), result.to_string()]
    );
    assert_eq!(bench.history(&result), history);
    assert_eq!(bench.held(&result).as_deref(), Some(source.id.as_str()));
    assert_eq!(bench.read_back(&result), pixels);
    assert_eq!(bench.count("merges"), 1);
    assert_eq!(bench.count("derived_sources"), 1);
    fs::rename(&offline, bench.directory.join("bracket")).unwrap();
    assert_eq!(bench.hashes(), hashes);
    bench.finish();
}

/// Remove from catalog moves a merge's result to Removed and deactivates its stack in one change,
/// leaving the developed input and every original as they were; Put back makes both active again;
/// the journal undoes either; Remove merge names the merge and does the same.
#[test]
fn hdr_merge_lifecycle_remove_and_put_back_move_the_result_with_its_stack() {
    let mut bench = Bench::new("remove", 3);
    let hashes = bench.hashes();
    let PublishedMerge {
        merge_id, result, ..
    } = bench.publish("create", &[0, 1, 2], 1);
    let history = bench.history(&result);
    let removed = bench.remove_as("agent", "remove", &result).unwrap();
    let detail = journal::inspect(
        &bench.service.connection,
        removed.answer().change.unwrap().0,
    )
    .unwrap();
    assert_eq!(detail.change.label, "Removed DSC_0002 HDR");
    assert_eq!(
        detail
            .rows
            .iter()
            .map(|row| (row.item.clone(), row.after.clone()))
            .collect::<Vec<_>>(),
        [
            (removal(&result), Some(json!({"removed_ms": 5_000}))),
            (stack(&merge_id), Some(json!({"state": "removed"}))),
        ]
    );
    assert_eq!(bench.removed_ms(&result), Some(5_000));
    assert!(bench.claimed().is_empty());
    assert_eq!(
        bench.removed_ms(&bench.developed),
        None,
        "the developed input stays"
    );
    let restored = bench.restore_as("agent", "restore", &result).unwrap();
    assert_eq!(restored.answer().items, 2);
    assert_eq!(bench.removed_ms(&result), None);
    assert_eq!(bench.claimed().len(), 3);
    // Undo Put back, then undo Remove: the result is removed again, then back.
    bench.undo_as("agent", "undo-restore").unwrap();
    assert_eq!(
        state(&bench.service.connection, &merge_id).unwrap(),
        Some(MergeState::Removed)
    );
    bench.undo_as("agent", "undo-remove").unwrap();
    assert_eq!(
        state(&bench.service.connection, &merge_id).unwrap(),
        Some(MergeState::Active)
    );
    assert_eq!(bench.removed_ms(&result), None);
    // Remove merge: the merge's result, through the same Remove.
    let targets = results(&bench.service.connection, std::slice::from_ref(&merge_id)).unwrap();
    assert_eq!(targets, [(result.clone(), bench.row(&result))]);
    bench
        .service
        .library_write(|tx| {
            remove::remove(tx, targets, 6_000)?.apply(tx, request("merge.remove", "agent", "rm"))
        })
        .unwrap();
    assert_eq!(
        state(&bench.service.connection, &merge_id).unwrap(),
        Some(MergeState::Removed)
    );
    // Removing a result already undone or removed leaves its merge's state as it is.
    let unchanged = bench.remove_as("agent", "remove-again", &result).unwrap();
    assert!(matches!(unchanged, Outcome::NoOp));
    assert_eq!(bench.history(&result), history);
    assert_eq!(bench.hashes(), hashes);
    bench.finish();
}

/// A second merge over an input an active stack claims is refused naming that stack, with nothing
/// written; once the first is removed it publishes; then putting back the first, or undoing its
/// removal, is refused naming the second, all or nothing, until the second is undone; and redoing
/// the second is refused in turn.
#[test]
fn hdr_merge_lifecycle_one_active_stack_per_input() {
    let mut bench = Bench::new("overlap", 5);
    let hashes = bench.hashes();
    let first = bench.publish("first", &[0, 1, 2], 1);
    let provenance = bench.provenance(&[2, 3, 4], DeghostMode::Auto);
    let source = bench.source(2);
    let counts = |bench: &Bench| {
        [
            "assets",
            "merges",
            "merge_inputs",
            "derived_sources",
            "asset_sources",
            "library_changes",
        ]
        .map(|table| bench.count(table))
    };
    let before = counts(&bench);
    let found = claims(
        &bench.service.connection,
        provenance.inputs.iter().map(|input| input.path.clone()),
    )
    .unwrap();
    assert_eq!(
        found,
        [StackClaim {
            path: bench.files[2].clone(),
            merge_id: first.merge_id.clone(),
            kind: MergeKind::Hdr,
            result_asset_id: first.result.clone(),
        }]
    );
    let error = bench
        .publish_as("agent", "second", &provenance, &source, |_, _, _| {
            Ok(Vec::new())
        })
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Conflict);
    assert_eq!(
        error.detail,
        format!(
            "DSC_0003.JPG is already in the HDR stack of DSC_0002 HDR ({}); remove that merge first",
            first.merge_id
        )
    );
    assert_eq!(
        error.data.as_ref().unwrap()["stack"],
        first.merge_id.as_str()
    );
    assert_eq!(counts(&bench), before, "nothing was written");
    // Removed by another client, the first no longer claims its inputs.
    bench
        .remove_as("desktop", "remove-first", &first.result)
        .unwrap();
    let second = bench
        .publish_as("agent", "second", &provenance, &source, |_, _, _| {
            Ok(Vec::new())
        })
        .unwrap();
    let observed = |bench: &Bench| {
        (
            bench.claimed(),
            bench.removed_ms(&first.result),
            state(&bench.service.connection, &first.merge_id).unwrap(),
            bench.count("library_changes"),
        )
    };
    let after = observed(&bench);
    assert_eq!(after.2, Some(MergeState::Removed));
    for refused in [
        bench.restore_as("desktop", "put-back", &first.result),
        bench.undo_as("desktop", "undo-remove"),
    ] {
        let error = refused.unwrap_err();
        assert_eq!(error.kind, ErrorKind::Conflict);
        assert!(
            error
                .detail
                .contains(&format!("HDR stack of DSC_0004 HDR ({})", second.merge_id)),
            "{error}"
        );
        assert_eq!(observed(&bench), after, "all or nothing");
    }
    // Once the second merge is undone, the first's removal can be undone.
    bench.undo_as("agent", "undo-second").unwrap();
    assert_eq!(
        state(&bench.service.connection, &second.merge_id).unwrap(),
        Some(MergeState::Undone)
    );
    bench.undo_as("desktop", "undo-remove-first").unwrap();
    assert_eq!(
        state(&bench.service.connection, &first.merge_id).unwrap(),
        Some(MergeState::Active)
    );
    assert_eq!(bench.removed_ms(&first.result), None);
    // Redoing the second's creation is refused now that the first claims an input again.
    let error = bench.redo_as("agent", "redo-second").unwrap_err();
    assert_eq!(error.kind, ErrorKind::Conflict);
    assert!(
        error.detail.contains("HDR stack of DSC_0002 HDR"),
        "{error}"
    );
    assert_eq!(
        state(&bench.service.connection, &second.merge_id).unwrap(),
        Some(MergeState::Undone)
    );
    assert_eq!(bench.hashes(), hashes);
    bench.finish();
}

/// Another actor's change to a merge's result makes the creating actor's undo a named conflict,
/// changing nothing.
#[test]
fn hdr_merge_lifecycle_another_actors_change_refuses_the_undo_by_name() {
    let mut bench = Bench::new("actors", 3);
    let PublishedMerge {
        merge_id, result, ..
    } = bench.publish("create", &[0, 1, 2], 1);
    bench.remove_as("desktop", "remove", &result).unwrap();
    let error = bench.undo_as("agent", "undo").unwrap_err();
    assert_eq!(error.kind, ErrorKind::Conflict);
    assert!(journal::names(&error, &removal(&result)), "{error:?}");
    assert!(journal::names(&error, &stack(&merge_id)), "{error:?}");
    assert_eq!(bench.removed_ms(&result), Some(5_000));
    assert_eq!(
        state(&bench.service.connection, &merge_id).unwrap(),
        Some(MergeState::Removed)
    );
    // Put back by the other actor: the values are the creation's again, but a later change is in
    // effect, so the undo is still refused.
    bench.restore_as("desktop", "restore", &result).unwrap();
    let error = bench.undo_as("agent", "undo-again").unwrap_err();
    assert_eq!(error.kind, ErrorKind::Conflict);
    assert_eq!(bench.removed_ms(&result), None);
    bench.finish();
}

/// An exact match is found by its inputs, provider and parameters: active, it is reused; removed,
/// it is reported for Put back; other parameters or a changed input match nothing.
#[test]
fn hdr_merge_lifecycle_exact_matches_are_found_active_or_removed() {
    let mut bench = Bench::new("matching", 3);
    let provenance = bench.provenance(&[0, 1, 2], DeghostMode::Auto);
    let key = provenance.match_key().unwrap();
    assert!(matching(&bench.service.connection, &key).unwrap().is_none());
    let PublishedMerge {
        merge_id, result, ..
    } = bench.publish("create", &[0, 1, 2], 1);
    let found = |bench: &Bench| matching(&bench.service.connection, &key).unwrap();
    assert_eq!(
        found(&bench),
        Some(MergeMatch {
            merge_id: merge_id.clone(),
            result_asset_id: result.clone(),
            state: MergeState::Active,
        })
    );
    bench.remove_as("agent", "remove", &result).unwrap();
    assert_eq!(found(&bench).unwrap().state, MergeState::Removed);
    let other = bench.provenance(&[0, 1, 2], DeghostMode::Off);
    assert_ne!(other.match_key().unwrap(), key);
    assert!(
        matching(&bench.service.connection, &other.match_key().unwrap())
            .unwrap()
            .is_none()
    );
    let mut changed = provenance.clone();
    changed.inputs[0].signature.modified_ns += 1;
    assert_ne!(changed.match_key().unwrap(), key);
    bench.finish();
}

/// Publication rechecks every input: a missing input, a changed one, an input photograph that is
/// gone or developed from another file, or a merge's own result as an input publishes nothing.
#[test]
fn hdr_merge_lifecycle_publication_rechecks_its_inputs() {
    let mut bench = Bench::new("recheck", 3);
    let hashes = bench.hashes();
    let source = bench.source(1);
    let base = bench.provenance(&[0, 1, 2], DeghostMode::Auto);
    let before = (bench.count("assets"), bench.count("merges"));
    let mut missing = base.clone();
    missing.inputs[0].path = bench.directory.join("bracket").join("DSC_9999.JPG");
    let mut changed = base.clone();
    changed.inputs[2].signature.len += 1;
    let mut gone = base.clone();
    gone.inputs[0].asset_id = Some(AssetId::new());
    let mut elsewhere = base.clone();
    elsewhere.inputs[0].asset_id = Some(bench.developed.clone());
    let mut twice = base.clone();
    twice.inputs[2].path = twice.inputs[0].path.clone();
    let mut kind = base.clone();
    kind.format = MERGE_PROVENANCE_FORMAT + 1;
    for (name, provenance, code, detail) in [
        (
            "missing",
            &missing,
            ErrorKind::SourceUnavailable,
            "DSC_9999.JPG is no longer at",
        ),
        (
            "changed",
            &changed,
            ErrorKind::Conflict,
            "DSC_0003.JPG changed since the merge read it",
        ),
        (
            "gone",
            &gone,
            ErrorKind::Conflict,
            "is no longer in the catalog",
        ),
        (
            "elsewhere",
            &elsewhere,
            ErrorKind::Conflict,
            "is no longer developed from DSC_0001.JPG",
        ),
        ("twice", &twice, ErrorKind::Validation, "is named twice"),
        (
            "format",
            &kind,
            ErrorKind::Incompatible,
            "provenance format 3 is not supported",
        ),
    ] {
        let error = bench
            .publish_as("agent", name, provenance, &source, |_, _, _| Ok(Vec::new()))
            .unwrap_err();
        assert_eq!(error.kind, code, "{name}: {error}");
        assert!(error.detail.contains(detail), "{name}: {error}");
        assert_eq!(
            (bench.count("assets"), bench.count("merges")),
            before,
            "{name}"
        );
    }
    // A merge's result is never an input: its photograph names a derived source, not a file.
    let first = bench.publish("first", &[0, 1, 2], 2);
    let error = refuse_generated(&bench.service.connection, &first.result).unwrap_err();
    assert!(
        error.detail.contains("a merge takes original files only"),
        "{error}"
    );
    refuse_generated(&bench.service.connection, &bench.developed).unwrap();
    bench.remove_as("agent", "remove", &first.result).unwrap();
    let mut nested = base.clone();
    nested.inputs[1].asset_id = Some(first.result.clone());
    let error = bench
        .publish_as(
            "agent",
            "nested",
            &nested,
            &source,
            |_, _, _| Ok(Vec::new()),
        )
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Validation, "{error}");
    assert_eq!(bench.hashes(), hashes);
    bench.finish();
}

/// Empty Removed deletes a removed merge's result with its provenance and stack, counting it,
/// releases its source only once no remaining photograph holds it, leaves every original as it
/// was and every lifted trigger in place, and makes a stale inverse naming the deleted merge a
/// conflict.
#[test]
fn hdr_merge_lifecycle_empty_removed_purges_only_the_result() {
    let mut bench = Bench::new("purge", 6);
    let hashes = bench.hashes();
    let triggers = |bench: &Bench| -> Vec<(String, String)> {
        bench
            .service
            .connection
            .prepare("SELECT name, sql FROM sqlite_schema WHERE type = 'trigger' ORDER BY name")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    let schema = triggers(&bench);
    // Two merges of different inputs share one source; a third holds its own.
    let shared = bench.source(1);
    let own = bench.source(2);
    let publish = |bench: &mut Bench, name: &str, inputs: &[usize], source: &PublishedSource| {
        let provenance = bench.provenance(inputs, DeghostMode::Auto);
        bench
            .publish_as("agent", name, &provenance, source, |_, _, _| Ok(Vec::new()))
            .unwrap()
    };
    let a = publish(&mut bench, "a", &[0, 1], &shared);
    let b = publish(&mut bench, "b", &[2, 3], &shared);
    let c = publish(&mut bench, "c", &[4, 5], &own);
    bench.remove_as("agent", "remove-a", &a.result).unwrap();
    bench.remove_as("agent", "remove-c", &c.result).unwrap();
    // Reopened, no source is live for having been published by this process.
    bench.reopen();
    let (emptied, collection) = bench.service.empty_removed(100).unwrap();
    assert_eq!(emptied.merges, 2);
    let mut deleted = emptied.assets.clone();
    deleted.sort();
    let mut expected = vec![a.result.clone(), c.result.clone()];
    expected.sort();
    assert_eq!(deleted, expected);
    assert_eq!(
        emptied.sources,
        std::slice::from_ref(&own.id),
        "the shared source is still held by b"
    );
    assert!(collection.is_some());
    assert_eq!(triggers(&bench), schema, "every lifted trigger is back");
    for (table, count) in [
        ("merges", 1),
        ("merge_inputs", 2),
        ("merge_stacks", 1),
        ("stack_claims", 2),
        ("derived_sources", 1),
        ("asset_sources", 1),
    ] {
        assert_eq!(bench.count(table), count, "{table}");
    }
    assert!(super::record(&bench.service.connection, &a.merge_id).is_err());
    assert_eq!(
        state(&bench.service.connection, &b.merge_id).unwrap(),
        Some(MergeState::Active)
    );
    assert_eq!(bench.held(&b.result).as_deref(), Some(shared.id.as_str()));
    bench.read_back(&b.result);
    // The developed input photograph is untouched; the originals are byte-identical.
    assert_eq!(bench.removed_ms(&bench.developed), None);
    assert_eq!(bench.hashes(), hashes);
    // Undoing the removal of c, whose merge is gone, is refused by name.
    let error = bench.undo_as("agent", "undo").unwrap_err();
    assert_eq!(error.kind, ErrorKind::Conflict);
    assert!(journal::names(&error, &stack(&c.merge_id)), "{error:?}");
    // The last holder of the shared source removed and emptied: the source goes with it.
    bench.remove_as("other", "remove-b", &b.result).unwrap();
    let (emptied, _) = bench.service.empty_removed(100).unwrap();
    assert_eq!(
        (emptied.merges, emptied.sources.as_slice()),
        (1, [shared.id.clone()].as_slice())
    );
    assert_eq!(bench.count("merges"), 0);
    assert_eq!(bench.count("stack_claims"), 0);
    assert_eq!(bench.hashes(), hashes);
    for file in &bench.files {
        assert!(file.is_file());
    }
    bench.finish();
}

/// Removing, and emptying from Removed, a catalog photograph that was a merge's input leaves the
/// merge and its result as they were, its provenance still naming the photograph it was.
#[test]
fn hdr_merge_lifecycle_an_input_photographs_removal_leaves_the_result() {
    let mut bench = Bench::new("input", 3);
    let hashes = bench.hashes();
    let PublishedMerge {
        merge_id, result, ..
    } = bench.publish("create", &[0, 1, 2], 1);
    let record = super::record(&bench.service.connection, &merge_id).unwrap();
    let developed = bench.developed.clone();
    bench
        .remove_as("agent", "remove-input", &developed)
        .unwrap();
    assert_eq!(bench.removed_ms(&result), None);
    assert_eq!(bench.claimed().len(), 3);
    let (emptied, _) = bench.service.empty_removed(100).unwrap();
    assert_eq!(
        (emptied.assets.as_slice(), emptied.merges),
        ([developed.clone()].as_slice(), 0)
    );
    assert_eq!(
        super::record(&bench.service.connection, &merge_id).unwrap(),
        record
    );
    assert_eq!(record.members[1].asset_id.as_ref(), Some(&developed));
    bench.service.state(&result).unwrap();
    bench.read_back(&result);
    assert_eq!(bench.hashes(), hashes);
    bench.finish();
}
