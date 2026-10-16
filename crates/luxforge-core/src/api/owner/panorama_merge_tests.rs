//! The panorama merge through the real catalog owner and the shared registry, with several clients
//! (`docs/design/panorama-merge.md`): planning from headers, creating, reusing, cancelling at every
//! stage, refusing before wasted work, a restart mid-merge, rerunning with other options,
//! inspecting, removing and restoring, stack claims across kinds, and the merge preview; then end
//! to end, a result past the resident limits published paged and reopened with its frames moved
//! away, and the production stitch's result against the reference.
//!
//! The job tests stitch with the tests' [`SimpleStitcher`], which is quick and exact by
//! construction; everything else — the frames' reads and spools, registration, the chunked durable
//! writer, the publication — is the production path. One end-to-end test runs the production
//! stitch (`crate::merge::panorama::stitching::production`) as the lane has it.
//! The job tests read their frames through a synthetic camera (`luxforge_reference::panorama`):
//! each file is real and read once, its SHA-256 is its identity, and its frame is the reference
//! scene recorded at its pose. The end-to-end tests write the scene as JPEG files read through the
//! shared frame path.
use super::*;
use crate::{
    LinearImage, ModuleRegistry,
    api::{
        ApiRequest, OwnerHandle,
        owner::{OwnerMessage, catalog::CatalogMessage, library::LibraryMessage},
    },
    catalog_types::{CameraBody, Dimensions, Exposure, HeaderMetadata},
    editor::{distinct_jpeg, mutation},
    merge::{
        hdr::SensorClipModel,
        job::{Developed, Development, Stage},
        panorama::{
            frames::{DevelopedFrame, FramePixels},
            stitching::simple::SimpleStitcher,
        },
    },
    source::{OpticalIdentity, SourceOptics},
};
use luxforge_raw::{OpticalEntry, OpticalLedger, OpticalStatus};
use luxforge_reference::panorama::{self as reference, Pose, Scene, Shot};
use luxforge_test_macros::slow;
use luxforge_testbase::Gate;
use std::{
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    thread::JoinHandle,
};

const W: usize = 320;
const H: usize = 240;
/// Each frame's horizontal field of view, and the turn between neighbours, in degrees: three frames
/// span about 120°, which Auto shows cylindrical.
const HFOV: f64 = 60.0;
const STEP: f64 = 30.0;
/// The scene's cell: about 17 pixels at the frames' 277-pixel focal length.
const CELL: f64 = 0.06;
/// The frames' 35 mm equivalent focal length: 36 mm across 320 pixels at 277 pixels.
const FOCAL_35MM: u16 = 31;
/// The scene's brightness in every frame, keeping it below the sensor's clipping.
const GAIN: f64 = 0.5;

/// One synthetic frame: its row's scene and where the camera pointed.
#[derive(Clone, Copy)]
struct Frame {
    seed: u64,
    pose: Pose,
}

/// The synthetic RAW camera the lane reads panorama frames with. It reads each file once, and
/// holds before it does while `reading` is shut, giving up when the job is cancelled.
struct Camera {
    frames: HashMap<PathBuf, Frame>,
    reading: Arc<Gate>,
    reads: AtomicUsize,
    recorded: Mutex<HashMap<PathBuf, Arc<Vec<f32>>>>,
}

fn lens() -> reference::Lens {
    reference::Lens::with_hfov(W, H, HFOV)
}

fn record(frame: Frame) -> reference::Frame {
    reference::record(
        &Scene::new(frame.seed, CELL),
        &lens(),
        &Shot {
            gain: [GAIN; 3],
            ..Shot::at(frame.pose)
        },
        None,
        0,
    )
}

fn header() -> HeaderMetadata {
    HeaderMetadata {
        camera: Some(CameraBody {
            make: "NIKON CORPORATION".into(),
            model: "NIKON Z 6".into(),
            serial: None,
        }),
        lens: Some("NIKKOR Z 24-70mm f/4 S".into()),
        exposure: Exposure {
            time_s: Some(1.0 / 250.0),
            f_number: Some(8.0),
            iso: Some(100),
            bias_ev: None,
            focal_mm: Some(24.0),
            focal_35mm_mm: Some(f32::from(FOCAL_35MM)),
        },
        dimensions: Some(Dimensions {
            width: W as u32,
            height: H as u32,
        }),
        ..HeaderMetadata::default()
    }
}

impl Camera {
    fn planes(&self, path: &Path, frame: Frame) -> Arc<Vec<f32>> {
        if let Some(planes) = self.recorded.lock().unwrap().get(path) {
            return planes.clone();
        }
        let recorded = record(frame);
        let planes: Arc<Vec<f32>> = Arc::new(
            recorded
                .rgb
                .iter()
                .flat_map(|plane| plane.iter().map(|&value| value as f32))
                .collect(),
        );
        self.recorded
            .lock()
            .unwrap()
            .insert(path.to_path_buf(), planes.clone());
        planes
    }
}

impl FrameDeveloper for Camera {
    fn develop(
        &self,
        path: &Path,
        gains: Option<[f32; 3]>,
        control: &JobControl,
    ) -> Result<DevelopedFrame, Error> {
        if !self.reading.pass_unless(|| control.is_cancelled()) {
            return Err(control.cancelled_error());
        }
        control.checkpoint()?;
        let bytes = fs::read(path)
            .map_err(|error| crate::atomic_file::file_error(path.display(), error.kind()))?;
        self.reads.fetch_add(1, Ordering::SeqCst);
        let fingerprint = format!("{:x}", Sha256::digest(&bytes));
        let frame = *self
            .frames
            .get(path)
            .ok_or_else(|| Error::unsupported_input(format!("{} is no frame", path.display())))?;
        let header = header();
        let gains = gains.unwrap_or([2.0, 1.0, 1.5]);
        let identity = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
        let entry = |status| OpticalEntry {
            status,
            provenance: "test".into(),
        };
        Ok(DevelopedFrame {
            developed: Developed {
                path: path.to_path_buf(),
                frame: FrameMetadata {
                    identity: fingerprint.clone(),
                    make: Some("NIKON CORPORATION".into()),
                    model: Some("NIKON Z 6".into()),
                    recording: Recording::Raw {
                        mode: "nikon-z6-14bit-lossless".into(),
                        layout: SensorLayout::Bayer,
                    },
                    width: W as u32,
                    height: H as u32,
                    orientation: 1,
                    exposure: job::settings(&header),
                    captured_at_ms: None,
                },
                fingerprint,
                header,
                place: None,
                volume: crate::index::volume_of(path, now_ms())?,
                development: None,
            },
            optics: SourceOptics {
                identity: OpticalIdentity {
                    make: Some("NIKON CORPORATION".into()),
                    model: Some("NIKON Z 6".into()),
                    lens_make: None,
                    lens_model: None,
                    focal_mm: Some(24.0),
                    focal_35mm: Some(FOCAL_35MM),
                },
                ledger: OpticalLedger {
                    distortion: entry(OpticalStatus::KnownUnapplied),
                    lateral_ca: entry(OpticalStatus::KnownUnapplied),
                    shading: entry(OpticalStatus::KnownUnapplied),
                    interpretation: "raw-mosaic:test".into(),
                },
            },
            pixels: FramePixels::Raw(Development {
                gains,
                cam_xyz: [identity[0], identity[1], identity[2], [0.0; 3]],
                image: LinearImage::new(W as u32, H as u32, (*self.planes(path, frame)).clone())?,
                clip: SensorClipModel::new(identity, gains, SensorLayout::Bayer)?,
            }),
        })
    }
}

/// A catalog beside rows of synthetic RAW frames, an ordinary photograph with an edit of its own, a
/// folder for the results, and the owner running with the synthetic camera and the simple stitch.
struct Bench {
    directory: PathBuf,
    catalog: PathBuf,
    /// Three files a row. The first row was shot right to left, the others left to right.
    files: Vec<PathBuf>,
    hashes: Vec<String>,
    photograph: AssetId,
    camera: Arc<Camera>,
    stitcher: Arc<SimpleStitcher>,
    owner: OwnerHandle,
    join: Option<JoinHandle<()>>,
    client: ClientId,
    folder: Value,
    root: PathBuf,
}

fn sha256(path: &Path) -> String {
    format!("{:x}", Sha256::digest(fs::read(path).unwrap()))
}

fn envelope(request: &str) -> Value {
    json!({"request_id": request, "actor": "test"})
}

fn start_owner(catalog: &Path) -> (OwnerHandle, JoinHandle<()>) {
    OwnerHandle::start_with(catalog, Arc::new(ModuleRegistry::builtin())).unwrap()
}

/// A catalog with one ordinary photograph edited, beside `directory`.
fn catalog_with_photograph(directory: &Path) -> (PathBuf, AssetId) {
    let catalog = directory.join("catalog.sqlite");
    let mut service = EditorService::open(&catalog).unwrap();
    let state = service
        .import(&distinct_jpeg(
            &luxforge_testbase::paths::jpeg(),
            &directory.join("photos").join("lake.jpg"),
        ))
        .unwrap();
    service
        .run_action(
            &state.asset.id,
            mutation(0, "lake-basic"),
            "set-basic",
            json!({"exposure": 0.5}),
        )
        .unwrap();
    (catalog, state.asset.id)
}

impl Bench {
    fn new(name: &str, rows: usize) -> Self {
        let directory = luxforge_testbase::paths::temp_dir(&format!("panorama-merge-{name}"))
            .canonicalize()
            .unwrap();
        let folder = directory.join("row");
        fs::create_dir_all(&folder).unwrap();
        let mut files = Vec::new();
        let mut frames = HashMap::new();
        for row in 0..rows {
            let mut poses = reference::row(3, STEP, 0.0);
            if row == 0 {
                poses.reverse();
            }
            for (position, pose) in poses.into_iter().enumerate() {
                let path = folder.join(format!("DSC_{:04}.NEF", 3 * row + position + 1));
                let mut bytes = format!("synthetic row {row} frame {position}\n").into_bytes();
                bytes.resize(1 << 16, (row * 3 + position) as u8);
                fs::write(&path, bytes).unwrap();
                frames.insert(
                    path.clone(),
                    Frame {
                        seed: 21 + row as u64,
                        pose,
                    },
                );
                files.push(path);
            }
        }
        let hashes = files.iter().map(|path| sha256(path)).collect();
        let (catalog, photograph) = catalog_with_photograph(&directory);
        let camera = Arc::new(Camera {
            frames,
            reading: Arc::new(Gate::new()),
            reads: AtomicUsize::new(0),
            recorded: Mutex::new(HashMap::new()),
        });
        let (owner, join) = start_owner(&catalog);
        let client = owner.register();
        let mut bench = Self {
            directory,
            catalog,
            files,
            hashes,
            photograph,
            camera,
            stitcher: SimpleStitcher::new(),
            owner,
            join: Some(join),
            client,
            folder: Value::Null,
            root: PathBuf::new(),
        };
        bench.install();
        bench.root = bench.on_owner(|owner| owner.service.artifact_root().to_path_buf());
        let created = bench.ok(
            "folder.create",
            json!({"name": "Merges", "mutation": envelope("folder")}),
        );
        bench.folder = created["folder"]["id"].clone();
        bench
    }

    /// Stitch with the bench's stitch and read with its camera.
    fn install(&self) {
        let (stitcher, camera) = (self.stitcher.clone(), self.camera.clone());
        self.lane(move |lane| lane.set_panorama(stitcher, Some(camera)));
    }

    /// Stitch with `stitcher` from now on.
    fn stitch_with(&mut self, stitcher: Arc<SimpleStitcher>) {
        self.stitcher = stitcher;
        self.install();
    }

    /// Change the merge lane on the owner, in order with every call after it.
    fn lane(&self, change: impl FnOnce(&mut MergeLane) + Send + 'static) {
        self.owner
            .sender
            .send(OwnerMessage::Catalog(CatalogMessage::Merge(
                LibraryMessage::Run(Box::new(move |owner| change(&mut owner.catalog.merge))),
            )))
            .unwrap();
    }

    /// Hold every merge from now on at `stage` until `gate` opens.
    fn hold_at(&self, stage: Stage, gate: Arc<Gate>) {
        self.lane(move |lane| {
            lane.hooks.stage = Some(Arc::new(move |reached| {
                if reached == stage {
                    gate.pass();
                }
            }));
        });
    }

    /// Record every stage a job reaches from now on.
    fn record_stages(&self) -> Arc<Mutex<Vec<Stage>>> {
        let stages = Arc::new(Mutex::new(Vec::new()));
        let seen = stages.clone();
        self.lane(move |lane| {
            lane.hooks.stage = Some(Arc::new(move |stage| seen.lock().unwrap().push(stage)));
        });
        stages
    }

    /// Stop the owner as Luxforge stops, and start it again on the same catalog.
    fn restart(&mut self) {
        self.owner.stop();
        self.join.take().unwrap().join().unwrap();
        let (owner, join) = start_owner(&self.catalog);
        self.owner = owner;
        self.join = Some(join);
        self.client = self.owner.register();
        self.install();
    }

    fn call(&self, client: ClientId, method: &str, params: Value) -> Result<Value, Value> {
        let response = self
            .owner
            .call(
                client,
                ApiRequest {
                    id: method.into(),
                    method: method.into(),
                    params,
                    token: None,
                },
            )
            .expect("the owner answered");
        match response.error {
            None => Ok(response.result.expect("a result")),
            Some(error) => Err(serde_json::to_value(error).unwrap()),
        }
    }

    fn ok(&self, method: &str, params: Value) -> Value {
        self.call(self.client, method, params)
            .unwrap_or_else(|error| panic!("{method}: {error}"))
    }

    fn failure(&self, method: &str, params: Value) -> Value {
        match self.call(self.client, method, params) {
            Ok(answer) => panic!("{method} answered {answer}"),
            Err(error) => error,
        }
    }

    fn paths(&self, row: usize) -> Vec<&PathBuf> {
        self.files[3 * row..3 * row + 3].iter().collect()
    }

    fn targets(&self, row: usize) -> Value {
        json!({"kind": "paths", "paths": self.paths(row)})
    }

    fn create_params(&self, row: usize, request: &str) -> Value {
        json!({
            "kind": "panorama",
            "targets": self.targets(row),
            "folder": {"kind": "existing", "folder_id": self.folder},
            "mutation": envelope(request),
        })
    }

    fn create(&self, row: usize, request: &str) -> Value {
        self.ok("merge.create", self.create_params(row, request))
    }

    fn job(&self, job_id: &Value) -> Value {
        self.ok("job.read", json!({"job_id": job_id}))
    }

    fn settled(&self, job_id: &Value) -> Value {
        luxforge_testbase::wait_for("the job to settle", || {
            let read = self.job(job_id);
            (!matches!(read["status"].as_str(), Some("queued" | "running"))).then_some(read)
        })
    }

    /// The result of a ready job.
    fn ready(&self, started: &Value) -> Value {
        let read = self.settled(&started["job_id"]);
        assert_eq!(read["status"], "ready", "{read}");
        read["result"].clone()
    }

    /// Create a panorama of one row and wait for its result.
    fn merged(&self, row: usize, request: &str) -> Value {
        self.ready(&self.create(row, request))
    }

    fn cancel(&self, job_id: &Value) {
        self.ok("job.cancel", json!({"job_id": job_id}));
    }

    fn reads(&self) -> usize {
        self.camera.reads.load(Ordering::SeqCst)
    }

    fn plans(&self) -> usize {
        self.stitcher.plans.load(Ordering::SeqCst)
    }

    /// Run `read` on the owner, in order with every call before it.
    fn on_owner<T: Send + 'static>(
        &self,
        read: impl FnOnce(&mut Owner) -> T + Send + 'static,
    ) -> T {
        let (answer, answered) = std::sync::mpsc::channel();
        self.owner
            .sender
            .send(OwnerMessage::Catalog(CatalogMessage::Merge(
                LibraryMessage::Run(Box::new(move |owner| {
                    let _ = answer.send(read(owner));
                })),
            )))
            .unwrap();
        answered.recv().unwrap()
    }

    fn count(&self, sql: &str) -> i64 {
        let sql = sql.to_owned();
        self.on_owner(move |owner| {
            owner
                .service
                .connection
                .query_row(&sql, [], |row| row.get::<_, i64>(0))
        })
        .unwrap()
    }

    fn merges(&self) -> PathBuf {
        self.root.join(MERGES)
    }

    /// Every spool file under the merges folder.
    fn spools(&self) -> Vec<PathBuf> {
        let Ok(folders) = fs::read_dir(self.merges()) else {
            return Vec::new();
        };
        folders
            .flatten()
            .filter_map(|folder| fs::read_dir(folder.path()).ok())
            .flat_map(|entries| entries.flatten().map(|entry| entry.path()))
            .filter(|path| job::is_spool(&path.file_name().unwrap().to_string_lossy()))
            .collect()
    }

    /// The unrelated photograph's row, recipe and history, and the originals' bytes: what no merge
    /// may change.
    fn untouched(&self) -> Vec<Vec<String>> {
        let photograph = self.photograph.clone();
        let mut rows = self.on_owner(move |owner| {
            let database = &owner.service.connection;
            let mut rows = Vec::new();
            for sql in [
                "SELECT * FROM assets WHERE id = ?1",
                "SELECT * FROM asset_state WHERE asset_id = ?1",
                "SELECT * FROM entries WHERE asset_id = ?1 ORDER BY sequence",
            ] {
                let mut statement = database.prepare(sql).unwrap();
                let columns = statement.column_count();
                let read = statement
                    .query_map([photograph.as_str()], |row| {
                        (0..columns)
                            .map(|at| Ok(format!("{:?}", row.get_ref(at)?)))
                            .collect::<Result<Vec<String>, rusqlite::Error>>()
                    })
                    .unwrap()
                    .collect::<Result<Vec<_>, _>>()
                    .unwrap();
                rows.extend(read);
            }
            rows
        });
        rows.push(self.files.iter().map(|path| sha256(path)).collect());
        rows
    }

    fn originals_unchanged(&self) {
        let now: Vec<String> = self.files.iter().map(|path| sha256(path)).collect();
        assert_eq!(now, self.hashes, "no original's bytes change");
    }
}

impl Drop for Bench {
    fn drop(&mut self) {
        self.camera.reading.open();
        self.owner.stop();
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
        let _ = fs::remove_dir_all(&self.directory);
    }
}

fn paths_of(inputs: &Value) -> Vec<String> {
    inputs
        .as_array()
        .unwrap()
        .iter()
        .map(|input| input["path"].as_str().unwrap().to_owned())
        .collect()
}

fn path_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// A panorama planned from headers, created into an existing folder, read while it runs, inspected
/// through the registry, retried and asked for again: one result named after the reference, its
/// stack left to right however the frames were shot, and nothing else changed.
#[test]
#[slow]
fn panorama_merge_job_plans_creates_and_reuses_one_result_through_the_registry() {
    let bench = Bench::new("create", 1);
    let before = bench.untouched();

    // Planning reads headers only; its size is provisional.
    let plan = bench.ok(
        "merge.plan",
        json!({"kind": "panorama", "targets": bench.targets(0)}),
    );
    assert_eq!(plan["kind"], "panorama");
    assert_eq!(plan["suitable"], true, "{plan}");
    assert!(plan["reference"].is_null(), "registration picks it: {plan}");
    assert_eq!(plan["inputs"].as_array().unwrap().len(), 3);
    assert_eq!(plan["provider"]["module"], "luxforge.panorama");
    assert_eq!(plan["provider"]["available"], true);
    assert_eq!(plan["parameters"]["panorama"]["projection"], "auto");
    assert_eq!(plan["parameters"]["panorama"]["boundary_warp"], 0);
    assert_eq!(plan["estimates"]["provisional"], true);
    assert_eq!(
        plan["estimates"]["temp_ceiling_bytes"],
        json!(MergeKind::Panorama.temp_ceiling())
    );
    assert_eq!(
        plan["estimates"]["scratch_limit_bytes"],
        json!(MergeKind::Panorama.scratch_limit())
    );
    assert_eq!(bench.reads(), 0, "planning reads no pixels");

    // Options out of range, or of another kind, are refused before anything starts.
    let warp = bench.failure(
        "merge.plan",
        json!({"kind": "panorama", "targets": bench.targets(0), "panorama": {"boundary_warp": 101}}),
    );
    assert_eq!(warp["code"], "validation", "{warp}");
    assert_eq!(warp["data"]["field"], "boundary_warp");
    let crossed = bench.failure(
        "merge.plan",
        json!({"kind": "panorama", "targets": bench.targets(0), "hdr": {"deghost": "off"}}),
    );
    assert_eq!(crossed["code"], "validation", "{crossed}");

    // Held as it matches, the job reports its stage, what it holds and its progress.
    let gate = Arc::new(Gate::new());
    gate.shut();
    bench.hold_at(Stage::Matching, gate.clone());
    let mut params = bench.create_params(0, "create");
    params["plan_id"] = plan["plan_id"].clone();
    let started = bench.ok("merge.create", params.clone());
    assert_eq!(started["deduplicated"], false);
    gate.wait_reached(1, "the merge's matching");
    let running = bench.job(&started["job_id"]);
    assert_eq!(running["status"], "running", "{running}");
    assert_eq!(running["kind"], "panorama-merge");
    assert_eq!(running["result"]["stage"]["stage"], "matching", "{running}");
    assert_eq!(running["progress"]["message"], "Matching", "{running}");
    let accounting = &running["result"]["accounting"];
    assert_eq!(
        accounting["temp_bytes"],
        json!(3 * (W * H) as u64 * job::SPOOL_PIXEL)
    );
    assert_eq!(
        accounting["scratch_limit_bytes"],
        json!(MergeKind::Panorama.scratch_limit())
    );
    assert!(
        accounting["scratch_peak_bytes"].as_u64().unwrap() <= MergeKind::Panorama.scratch_limit()
    );
    assert_eq!(bench.spools().len(), 3, "one spool file a frame");
    // The same request again is the same job, and so is the same merge asked for anew, in another
    // order.
    assert_eq!(
        bench.ok("merge.create", params.clone())["job_id"],
        started["job_id"]
    );
    let mut reordered = bench.create_params(0, "create-reordered");
    reordered["targets"]["paths"] = json!([bench.files[2], bench.files[0], bench.files[1]]);
    let again = bench.ok("merge.create", reordered);
    assert_eq!(
        (&again["job_id"], &again["deduplicated"]),
        (&started["job_id"], &json!(true))
    );
    gate.open();
    let result = bench.ready(&started);
    assert_eq!(result["kind"], "panorama");
    assert_eq!(result["file_name"], "DSC_0002 Panorama", "{result}");
    assert_eq!(result["merged"], true);
    assert_eq!(result["inputs"], 3);
    assert_eq!(result["reused"], false);
    assert_eq!(result["catalog_folder_id"], bench.folder);
    assert!(result.get("exposure_range_ev").is_none(), "{result}");
    // About 120° across: Auto shows it cylindrical.
    assert_eq!(result["projection"], "cylindrical", "{result}");
    let across = result["field_of_view_deg"][0].as_f64().unwrap();
    assert!((115.0..125.0).contains(&across), "{result}");
    assert!(result["change"].is_u64(), "one library change: {result}");
    assert_eq!(result["accounting"]["temp_bytes"], 0, "the spool is gone");
    assert_eq!(bench.reads(), 3, "each frame read once");
    assert!(bench.spools().is_empty());

    // Inspection through the registry: the stack left to right, the frames shot right to left.
    let inspected = bench.ok("merge.inspect", json!({"merge_id": result["merge_id"]}));
    assert_eq!(
        bench.ok(
            "merge.inspect",
            json!({"asset_id": result["result_asset_id"]})
        ),
        inspected
    );
    assert_eq!(inspected["kind"], "panorama");
    assert_eq!(inspected["state"], "active");
    assert_eq!(inspected["source"]["status"], "available", "{inspected}");
    assert_eq!(inspected["rerunnable"], true);
    assert_eq!(inspected["projection"], "cylindrical");
    assert_eq!(
        paths_of(&inspected["inputs"]),
        [2, 1, 0].map(|at| path_string(&bench.files[at]))
    );
    assert_eq!(
        paths_of(&inspected["provenance"]["inputs"]),
        [2, 1, 0].map(|at| path_string(&bench.files[at]))
    );
    let resolved = &inspected["provenance"]["resolved"]["panorama"];
    assert_eq!(resolved["reference"], 1, "the middle frame: {resolved}");
    assert_eq!(
        inspected["provider"]["version"],
        crate::merge::providers::PANORAMA_VERSION
    );
    // No lens profile was baked: the result is a derived source of its own.
    assert_eq!(
        inspected["optics"],
        serde_json::to_value(crate::SourceOptics::derived_ledger()).unwrap()
    );
    for input in inspected["inputs"].as_array().unwrap() {
        assert_eq!(input["file"], "available");
        assert!(input.get("offset_ev").is_none(), "{input}");
    }
    // The result is the crop, at the reference's scale, with the reference's camera.
    let asset = result["result_asset_id"].as_str().unwrap().to_owned();
    let (width, height, make): (i64, i64, String) = bench
        .on_owner(move |owner| {
            owner.service.connection.query_row(
                "SELECT a.width, a.height, c.make FROM assets a
                 JOIN capture c ON c.asset_row = a.row_id WHERE a.id = ?1",
                [asset],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
        })
        .unwrap();
    let crop = &resolved["stitch"]["crop"];
    assert_eq!(
        (json!(width), json!(height)),
        (crop["width"].clone(), crop["height"].clone())
    );
    assert!(
        width > W as i64 * 3 / 2 && height > H as i64 * 3 / 4,
        "{width} × {height}"
    );
    assert_eq!(make, "NIKON CORPORATION");

    // A retry answers the published merge; a new request reuses it, reading nothing.
    let retried = bench.ok("merge.create", params);
    assert_eq!(retried["deduplicated"], true);
    assert_eq!(bench.ready(&retried)["merge_id"], result["merge_id"]);
    let reused = bench.merged(0, "create-once-more");
    assert_eq!(reused["merge_id"], result["merge_id"]);
    assert_eq!(reused["reused"], true);
    assert_eq!(bench.reads(), 3);
    assert_eq!(bench.count("SELECT COUNT(*) FROM merges"), 1);

    assert_eq!(bench.untouched(), before);
    bench.originals_unchanged();
}

/// A merge cancelled while it reads, spools, matches, stitches or saves ends `cancelled` with
/// nothing published and no spool left; its retry answers how it ended, and a new request merges.
#[test]
#[slow]
fn panorama_merge_job_cancels_at_every_stage_leaving_nothing() {
    let bench = Bench::new("cancel", 1);
    let before = bench.untouched();

    // While reading: the development itself gives up.
    bench.camera.reading.shut();
    let reading = bench.create(0, "cancel-reading");
    bench.camera.reading.wait_reached(1, "the first read");
    bench.cancel(&reading["job_id"]);
    assert_eq!(bench.settled(&reading["job_id"])["status"], "cancelled");
    bench.camera.reading.open();

    for (at, stage) in [
        Stage::Spooled(1),
        Stage::Reading(2),
        Stage::Matching,
        Stage::Stitching(0),
        Stage::Saved,
    ]
    .into_iter()
    .enumerate()
    {
        let gate = Arc::new(Gate::new());
        gate.shut();
        bench.hold_at(stage, gate.clone());
        let started = bench.create(0, &format!("cancel-{at}"));
        gate.wait_reached(1, "the held stage");
        bench.cancel(&started["job_id"]);
        gate.open();
        let read = bench.settled(&started["job_id"]);
        assert_eq!(read["status"], "cancelled", "{stage:?}: {read}");
        assert_eq!(bench.count("SELECT COUNT(*) FROM merges"), 0, "{stage:?}");
        assert!(bench.spools().is_empty(), "{stage:?}: {:?}", bench.spools());
        let retry = bench.create(0, &format!("cancel-{at}"));
        assert_eq!(retry["deduplicated"], true);
        assert_eq!(bench.job(&retry["job_id"])["status"], "cancelled");
    }
    bench.lane(|lane| lane.hooks.stage = None);
    assert_eq!(bench.merged(0, "after")["inputs"], 3);
    assert_eq!(bench.count("SELECT COUNT(*) FROM merges"), 1);
    assert_eq!(bench.untouched(), before);
    bench.originals_unchanged();
}

/// The temporary ceiling, the free space, a full disk and the scratch limit are refused once the
/// first frame's size is known, before another frame is read; a result whose objects would not
/// fit, and a stitch that refuses its size, are refused before any chunk is stitched. Each names
/// what it refuses, leaves no spool, and a merge within the limits then completes.
#[test]
#[slow]
fn panorama_merge_job_refuses_before_wasted_work() {
    let mut bench = Bench::new("limits", 1);
    let spooled = 3 * (W * H) as u64 * job::SPOOL_PIXEL;
    type Limit = Box<dyn Fn(&mut MergeLane) + Send + Sync>;
    let cases: [(&str, Limit, &str); 4] = [
        (
            "ceiling",
            Box::new(|lane| lane.limits_mut(MergeKind::Panorama).temp_ceiling = 1 << 20),
            "ceiling_bytes",
        ),
        (
            "free-space",
            Box::new(|lane| lane.limits_mut(MergeKind::Panorama).available = Some(1 << 20)),
            "available_bytes",
        ),
        (
            "disk-full",
            Box::new(|lane| lane.hooks.spool_full = Some(1)),
            "",
        ),
        (
            "scratch",
            Box::new(|lane| lane.limits_mut(MergeKind::Panorama).scratch = 1 << 20),
            "",
        ),
    ];
    for (name, limit, datum) in cases {
        let reads = bench.reads();
        let limit = Arc::new(limit);
        let set = limit.clone();
        bench.lane(move |lane| set(lane));
        let started = bench.create(0, name);
        let read = bench.settled(&started["job_id"]);
        assert_eq!(read["status"], "failed", "{name}: {read}");
        assert_eq!(read["error"]["code"], "resource-limit", "{name}: {read}");
        if !datum.is_empty() {
            assert!(read["error"]["data"][datum].is_u64(), "{name}: {read}");
        }
        assert!(
            bench.reads() - reads <= 1,
            "{name}: refused once the first frame is read"
        );
        assert!(bench.spools().is_empty(), "{name}");
        bench.lane(|lane| {
            *lane.limits_mut(MergeKind::Panorama) = Limits::of(MergeKind::Panorama);
            lane.hooks = Hooks::default();
        });
    }

    // Room for the frames' spool but not the result's objects: refused once the stitch is
    // planned, before any chunk.
    let stages = bench.record_stages();
    bench.lane(move |lane| lane.limits_mut(MergeKind::Panorama).temp_ceiling = spooled + 1024);
    let started = bench.create(0, "result-size");
    let read = bench.settled(&started["job_id"]);
    assert_eq!(read["error"]["code"], "resource-limit", "{read}");
    assert!(stages.lock().unwrap().contains(&Stage::Matching));
    assert!(
        !stages
            .lock()
            .unwrap()
            .iter()
            .any(|stage| matches!(stage, Stage::Stitching(_))),
        "no chunk is stitched: {stages:?}"
    );
    assert!(bench.spools().is_empty());
    bench.lane(|lane| *lane.limits_mut(MergeKind::Panorama) = Limits::of(MergeKind::Panorama));

    // A stitch that cannot fit the output is refused by file name, before any chunk.
    stages.lock().unwrap().clear();
    bench.stitch_with(Arc::new(SimpleStitcher {
        refuse: true,
        ..SimpleStitcher::default()
    }));
    let started = bench.create(0, "too-large");
    let read = bench.settled(&started["job_id"]);
    assert_eq!(read["status"], "failed", "{read}");
    assert_eq!(read["error"]["code"], "validation", "{read}");
    assert_eq!(read["error"]["data"]["kind"], "panorama");
    assert_eq!(read["error"]["data"]["reason"], "output-too-large");
    assert!(
        read["error"]["message"]
            .as_str()
            .unwrap()
            .contains("DSC_0001.NEF"),
        "named by file: {read}"
    );
    assert!(
        !stages
            .lock()
            .unwrap()
            .iter()
            .any(|stage| matches!(stage, Stage::Stitching(_))),
        "{stages:?}"
    );
    assert!(bench.spools().is_empty());
    assert_eq!(bench.count("SELECT COUNT(*) FROM merges"), 0);

    bench.stitch_with(SimpleStitcher::new());
    assert_eq!(bench.merged(0, "fits")["inputs"], 3);
    bench.originals_unchanged();
}

/// Across a restart, a merge the process stopped in answers `interrupted` and publishes nothing;
/// a new request merges.
#[test]
#[slow]
fn panorama_merge_job_restart_interrupts_and_a_new_request_merges() {
    let mut bench = Bench::new("restart", 1);
    bench.camera.reading.shut();
    let interrupted = bench.create(0, "interrupted");
    bench.camera.reading.wait_reached(1, "the read");
    bench.restart();
    bench.camera.reading.open();
    let retry = bench.create(0, "interrupted");
    let read = bench.job(&retry["job_id"]);
    assert_eq!(read["status"], "cancelled", "{read}");
    assert_eq!(read["error"]["data"]["reason"], "interrupted", "{read}");
    assert_ne!(retry["job_id"], interrupted["job_id"]);
    assert_eq!(bench.count("SELECT COUNT(*) FROM merges"), 0);
    let result = bench.merged(0, "after-restart");
    assert!(bench.spools().is_empty());
    bench.restart();
    let retry = bench.create(0, "after-restart");
    assert_eq!(bench.ready(&retry)["merge_id"], result["merge_id"]);
}

/// A rerun with another Boundary Warp and projection merges again and replaces the stack; an exact
/// rerun reuses the source; Remove and Put back work as HDR's; a rerun of another kind is refused;
/// and a stack's originals are claimed against a merge of either kind, naming the stack.
#[test]
#[slow]
fn panorama_merge_job_reruns_removes_restores_and_claims_across_kinds() {
    let bench = Bench::new("rerun", 1);
    let first = bench.merged(0, "create");
    let reads = bench.reads();

    let rerun = bench.ok(
        "merge.rerun",
        json!({"merge_id": first["merge_id"],
               "panorama": {"boundary_warp": 40, "projection": "spherical"},
               "mutation": envelope("rerun")}),
    );
    let second = bench.ready(&rerun);
    assert_eq!(second["replaced"], first["merge_id"]);
    assert_eq!(second["projection"], "spherical");
    assert_ne!(second["result_asset_id"], first["result_asset_id"]);
    assert_eq!(bench.reads(), reads + 3);
    assert_eq!(
        bench.ok("merge.inspect", json!({"merge_id": first["merge_id"]}))["state"],
        "removed"
    );
    let new = bench.ok("merge.inspect", json!({"merge_id": second["merge_id"]}));
    assert_eq!(new["parameters"]["panorama"]["boundary_warp"], 40, "{new}");
    assert_eq!(new["parameters"]["panorama"]["projection"], "spherical");

    // Another kind's options, or another kind, are refused.
    let crossed = bench.failure(
        "merge.rerun",
        json!({"merge_id": second["merge_id"], "hdr": {"deghost": "off"},
               "mutation": envelope("crossed")}),
    );
    assert_eq!(crossed["code"], "validation", "{crossed}");
    let kind = bench.failure(
        "merge.rerun",
        json!({"merge_id": second["merge_id"], "kind": "hdr", "mutation": envelope("kind")}),
    );
    assert_eq!(kind["code"], "validation", "{kind}");
    assert_eq!(
        kind["data"],
        json!({"kind": "panorama", "requested": "hdr"})
    );

    // An exact rerun gives the new result the same source, merging nothing.
    let exact = bench.ok(
        "merge.rerun",
        json!({"merge_id": second["merge_id"], "mutation": envelope("exact")}),
    );
    let third = bench.ready(&exact);
    assert_eq!(third["source_id"], second["source_id"]);
    assert_eq!(bench.reads(), reads + 3);

    // The stack claims its originals against either kind, naming the stack.
    let plan = bench.ok(
        "merge.plan",
        json!({"kind": "panorama", "targets": bench.targets(0)}),
    );
    assert_eq!(plan["claimed"].as_array().unwrap().len(), 3, "{plan}");
    let hdr = bench.failure(
        "merge.create",
        json!({"kind": "hdr", "targets": bench.targets(0),
               "folder": {"kind": "existing", "folder_id": bench.folder},
               "mutation": envelope("hdr-of-a-panorama")}),
    );
    assert_eq!(hdr["code"], "conflict", "{hdr}");
    assert_eq!(hdr["data"]["stack"], third["merge_id"], "{hdr}");
    let mut other = bench.create_params(0, "other-warp");
    other["panorama"] = json!({"boundary_warp": 10});
    let claimed = bench.failure("merge.create", other);
    assert_eq!(claimed["code"], "conflict", "{claimed}");
    assert_eq!(claimed["data"]["stack"], third["merge_id"]);

    // Remove and Put back; the first cannot come back while the third claims its frames.
    let removed = bench.ok(
        "merge.remove",
        json!({"merge_ids": [third["merge_id"]], "mutation": envelope("remove")}),
    );
    assert_eq!(removed["outcome"], "applied", "{removed}");
    bench.ok(
        "merge.restore",
        json!({"merge_ids": [third["merge_id"]], "mutation": envelope("restore")}),
    );
    assert_eq!(
        bench.ok("merge.inspect", json!({"merge_id": third["merge_id"]}))["state"],
        "active"
    );
    let blocked = bench.failure(
        "merge.restore",
        json!({"merge_ids": [first["merge_id"]], "mutation": envelope("restore-first")}),
    );
    assert_eq!(blocked["code"], "conflict", "{blocked}");
    bench.originals_unchanged();
}

#[test]
#[slow]
fn panorama_merge_job_rerun_retry_after_restart_returns_the_new_result() {
    let mut bench = Bench::new("rerun-retry", 1);
    let first = bench.merged(0, "create");
    let params = json!({"merge_id": first["merge_id"],
                        "panorama": {"boundary_warp": 40, "projection": "spherical"},
                        "mutation": envelope("rerun")});
    let started = bench.ok("merge.rerun", params.clone());
    let second = bench.ready(&started);
    assert_ne!(second["merge_id"], first["merge_id"]);
    let reads = bench.reads();
    bench.restart();
    let retried = bench.ok("merge.rerun", params);
    let retry = bench.ready(&retried);
    for field in ["merge_id", "result_asset_id", "source_id"] {
        assert_eq!(retry[field], second[field], "{field}: {retry}");
    }
    assert_eq!(retry["deduplicated"], true);
    assert_eq!(bench.reads(), reads, "retry reads and merges nothing again");
    bench.originals_unchanged();
}

/// A panorama's preview reads each frame once and answers a picture with its projection, field of
/// view and overlays in the stack's order; a projection change redraws without reading or
/// registering, a reference change registers again on the retained frames; and a merge of the
/// preview reads nothing.
#[test]
#[slow]
fn panorama_merge_job_preview_redraws_without_reading_and_merges_from_it() {
    let bench = Bench::new("preview", 1);
    let started = bench.ok(
        "merge.preview",
        json!({"kind": "panorama", "targets": bench.targets(0)}),
    );
    let first = bench.ready(&started);
    assert_eq!(bench.reads(), 3, "each frame read once");
    assert_eq!(first["kind"], "panorama");
    assert_eq!(first["revision"], 1);
    assert!(first.get("refusal").is_none(), "{first}");
    let path = PathBuf::from(first["path"].as_str().unwrap());
    assert!(path.exists());
    let (width, height) = (
        first["width"].as_u64().unwrap(),
        first["height"].as_u64().unwrap(),
    );
    assert!(width > height && width <= 1536, "{first}");
    let diagnostics = &first["diagnostics"]["panorama"];
    assert_eq!(diagnostics["projection"], "cylindrical", "{diagnostics}");
    assert_eq!(diagnostics["reference"], 1);
    assert_eq!(diagnostics["proxy_side"], W as u64);
    let overlays = &first["overlays"]["panorama"];
    let frames = overlays["frames"].as_array().unwrap();
    assert_eq!(
        frames
            .iter()
            .map(|frame| frame["frame"].clone())
            .collect::<Vec<_>>(),
        [json!(0), json!(1), json!(2)]
    );
    // In the stack's order, left to right: the first outline's left edge is leftmost.
    let left = |frame: &Value| {
        frame["outline"]
            .as_array()
            .unwrap()
            .iter()
            .map(|point| point[0].as_f64().unwrap())
            .fold(f64::MAX, f64::min)
    };
    assert!(left(&frames[0]) < left(&frames[1]) && left(&frames[1]) < left(&frames[2]));
    assert_eq!(overlays["crop"].as_array().unwrap().len(), 4);
    assert!(
        first["output"]["width"].as_u64().unwrap() > W as u64 * 3 / 2,
        "{first}"
    );

    // A projection change redraws: no read, no registration, one plan.
    let plans = bench.plans();
    let stages = bench.record_stages();
    let update = |panorama: Value| {
        let started = bench.ok(
            "merge.preview-update",
            json!({"preview_id": first["preview_id"], "panorama": panorama}),
        );
        bench.ready(&started)
    };
    let spherical = update(json!({"projection": "spherical"}));
    assert_eq!(spherical["revision"], 2);
    assert_eq!(
        spherical["diagnostics"]["panorama"]["projection"],
        "spherical"
    );
    assert_eq!(
        spherical["diagnostics"]["panorama"]["max_residual_px"],
        diagnostics["max_residual_px"]
    );
    assert_eq!(bench.plans(), plans + 1);
    assert_eq!(bench.reads(), 3);
    assert!(
        !stages
            .lock()
            .unwrap()
            .iter()
            .any(|stage| matches!(stage, Stage::Reading(_))),
        "{stages:?}"
    );

    // Another reference registers again on the retained frames: the leftmost frame, the file
    // shot last.
    let leftmost = update(json!({"projection": "spherical", "reference": bench.files[2]}));
    assert_eq!(leftmost["revision"], 3);
    assert_eq!(
        leftmost["diagnostics"]["panorama"]["reference"], 0,
        "{leftmost}"
    );
    assert_eq!(bench.reads(), 3);

    // The merge takes the preview over and reads nothing.
    let mut params = bench.create_params(0, "from-preview");
    params["preview_id"] = first["preview_id"].clone();
    params["panorama"] = json!({"projection": "spherical", "reference": bench.files[2]});
    let result = bench.ready(&bench.ok("merge.create", params));
    assert_eq!(result["file_name"], "DSC_0003 Panorama", "{result}");
    assert_eq!(result["projection"], "spherical");
    assert_eq!(bench.reads(), 3, "the merge reads nothing");
    let inspected = bench.ok("merge.inspect", json!({"merge_id": result["merge_id"]}));
    assert_eq!(
        inspected["provenance"]["resolved"]["panorama"]["reference"], 0,
        "{inspected}"
    );
    assert_eq!(inspected["parameters"]["panorama"]["reference"], 0);
    bench.originals_unchanged();
}

// End to end.

/// Write `frame` as an sRGB JPEG at `path`.
fn write_jpeg(path: &Path, frame: &reference::Frame) {
    let mut rgba = Vec::with_capacity(frame.width * frame.height * 4);
    for y in 0..frame.height {
        for x in 0..frame.width {
            let rgb = frame.at(x, y);
            rgba.extend(rgb.map(luxforge_reference::srgb::code));
            rgba.push(255);
        }
    }
    let mut out = Vec::new();
    luxforge_jpeg::encode(
        &mut out,
        frame.width as u32,
        frame.height as u32,
        &rgba,
        &luxforge_jpeg::Settings {
            quality: 100,
            chroma: (1, 1),
            optimise: false,
            pixels_per_inch: None,
            segments: &[],
            icc: None,
        },
        &mut |_| Ok::<(), luxforge_jpeg::JpegError>(()),
    )
    .unwrap();
    fs::write(path, out).unwrap();
}

/// A row of JPEG frames read through the shared frame path, stitched larger than a resident source
/// may be (128 MP), published paged in one transaction, and reopened after a restart with its
/// frames moved away: the same samples, its originals unchanged and inspection reporting them
/// missing.
#[test]
#[slow]
fn panorama_merge_end_to_end_publishes_a_paged_result_that_reopens_without_its_frames() {
    let directory = luxforge_testbase::paths::temp_dir("panorama-merge-end-to-end")
        .canonicalize()
        .unwrap();
    let row = directory.join("row");
    fs::create_dir_all(&row).unwrap();
    let paths: Vec<PathBuf> = reference::row(3, STEP, 0.0)
        .into_iter()
        .enumerate()
        .map(|(at, pose)| {
            let path = row.join(format!("IMG_{:04}.JPG", at + 1));
            write_jpeg(&path, &record(Frame { seed: 31, pose }));
            path
        })
        .collect();
    let hashes: Vec<String> = paths.iter().map(|path| sha256(path)).collect();
    let (catalog, _) = catalog_with_photograph(&directory);
    let start = || {
        let (owner, join) = start_owner(&catalog);
        // The shared frame path; only the stitch is the tests', magnified past 128 MP.
        let stitcher: Arc<dyn Stitcher> = SimpleStitcher::magnified(36.0);
        owner
            .sender
            .send(OwnerMessage::Catalog(CatalogMessage::Merge(
                LibraryMessage::Run(Box::new(move |owner| {
                    owner.catalog.merge.set_panorama(stitcher, None);
                })),
            )))
            .unwrap();
        (owner, join)
    };
    let on_owner = |owner: &OwnerHandle, read: Box<dyn FnOnce(&mut Owner) + Send>| {
        owner
            .sender
            .send(OwnerMessage::Catalog(CatalogMessage::Merge(
                LibraryMessage::Run(read),
            )))
            .unwrap();
    };
    let call = |owner: &OwnerHandle, client: ClientId, method: &str, params: Value| {
        let response = owner
            .call(
                client,
                ApiRequest {
                    id: method.into(),
                    method: method.into(),
                    params,
                    token: None,
                },
            )
            .expect("the owner answered");
        match response.error {
            None => response.result.expect("a result"),
            Some(error) => panic!("{method}: {}", serde_json::to_value(error).unwrap()),
        }
    };
    let settled = |owner: &OwnerHandle, client: ClientId, job: &Value| {
        let read = luxforge_testbase::wait_for("the job to settle", || {
            let read = call(owner, client, "job.read", json!({"job_id": job["job_id"]}));
            (!matches!(read["status"].as_str(), Some("queued" | "running"))).then_some(read)
        });
        assert_eq!(read["status"], "ready", "{read}");
        read["result"].clone()
    };
    // The level-0 window at the result's centre, read from the prepared paged source.
    let window = |owner: &OwnerHandle, client: ClientId, asset: &Value| {
        settled(
            owner,
            client,
            &call(owner, client, "source.prepare", json!({"asset_id": asset})),
        );
        let asset = AssetId::parse(asset.as_str().unwrap().to_owned()).unwrap();
        let (answer, answered) = std::sync::mpsc::channel();
        on_owner(
            owner,
            Box::new(move |owner| {
                let state = owner.service.state(&asset).unwrap();
                let read = owner.service.derived_source_read(&asset).unwrap();
                let _ = answer.send((state.asset.width, state.asset.height, read));
            }),
        );
        let (width, height, read) = answered.recv().unwrap();
        use crate::artifacts::derived_source::{
            SourceClass,
            paged_source::{PagedSource, WindowRequest},
            read_manifest,
        };
        let manifest = read_manifest(&read).unwrap();
        assert_eq!(
            manifest.class(),
            SourceClass::Paged,
            "past the resident limits"
        );
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let paged = PagedSource::open(read, manifest, &cancel).unwrap();
        let window = paged
            .window(
                WindowRequest {
                    level: 0,
                    rect: [width / 2 - 64, height / 2 - 64, 128, 128],
                    halo: 0,
                },
                &cancel,
            )
            .unwrap();
        (width, height, window.planes)
    };

    let (owner, join) = start();
    let client = owner.register();
    let plan = call(
        &owner,
        client,
        "merge.plan",
        json!({"kind": "panorama", "targets": {"kind": "paths", "paths": paths}}),
    );
    assert_eq!(plan["suitable"], true, "{plan}");
    let created = call(
        &owner,
        client,
        "merge.create",
        json!({"kind": "panorama", "targets": {"kind": "paths", "paths": paths},
               "plan_id": plan["plan_id"], "mutation": envelope("create")}),
    );
    let result = settled(&owner, client, &created);
    assert_eq!(result["file_name"], "IMG_0002 Panorama", "{result}");
    let asset = result["result_asset_id"].clone();
    let (width, height, before) = window(&owner, client, &asset);
    assert!(
        u64::from(width) * u64::from(height) > 128_000_000,
        "{width} × {height}"
    );
    assert!(before.iter().all(|sample| sample.is_finite()));
    assert!(
        before.iter().any(|sample| *sample > 0.01),
        "the centre is covered"
    );
    owner.stop();
    join.join().unwrap();

    // The frames move away while Luxforge is stopped; the result reopens from its own source.
    let away = directory.join("away");
    fs::rename(&row, &away).unwrap();
    let (owner, join) = start();
    let client = owner.register();
    let (again_width, again_height, after) = window(&owner, client, &asset);
    assert_eq!((again_width, again_height), (width, height));
    assert!(
        before
            .iter()
            .zip(&after)
            .all(|(a, b)| a.to_bits() == b.to_bits()),
        "the same samples"
    );
    let inspected = call(
        &owner,
        client,
        "merge.inspect",
        json!({"merge_id": result["merge_id"]}),
    );
    assert_eq!(inspected["source"]["status"], "available", "{inspected}");
    for input in inspected["inputs"].as_array().unwrap() {
        assert_eq!(input["file"], "missing", "{input}");
    }
    assert_eq!(inspected["rerunnable"], false);
    owner.stop();
    join.join().unwrap();
    let moved: Vec<String> = paths
        .iter()
        .map(|path| sha256(&away.join(path.file_name().unwrap())))
        .collect();
    assert_eq!(moved, hashes, "no original's bytes change");
    let _ = fs::remove_dir_all(&directory);
}

/// A result's sample lies between the values the frames recording it hold there (the scene along
/// each frame pixel's true ray, in 8-bit sRGB), to within this relative to the lower plus a floor
/// of [`STITCHED_FLOOR`] on average, about two JPEG codes...
const STITCHED_MEAN: f64 = 0.02;
const STITCHED_FLOOR: f64 = 0.02;
/// ...and within this at all but [`STITCHED_OUTLIERS`] of its pixels.
const STITCHED_PIXEL: f64 = 0.1;
const STITCHED_OUTLIERS: f64 = 0.001;

/// A row of JPEG frames through the shared frame path, registration and the production stitch:
/// the published result's size and crop are the reference's layout (`luxforge_reference::panorama`)
/// of the registered geometry the provenance records, and each of its pixels shows what the frames
/// recording that direction hold there.
#[test]
#[slow]
fn panorama_merge_end_to_end_stitches_the_scene_through_the_production_stitch() {
    let directory = luxforge_testbase::paths::temp_dir("panorama-merge-end-to-end-stitch")
        .canonicalize()
        .unwrap();
    let row = directory.join("row");
    fs::create_dir_all(&row).unwrap();
    const SEED: u64 = 37;
    let poses = reference::row(3, STEP, 0.0);
    let paths: Vec<PathBuf> = poses
        .iter()
        .enumerate()
        .map(|(at, &pose)| {
            let path = row.join(format!("IMG_{:04}.JPG", at + 1));
            write_jpeg(&path, &record(Frame { seed: SEED, pose }));
            path
        })
        .collect();
    let (catalog, _) = catalog_with_photograph(&directory);
    // The owner as built: the production stitch and the shared frame path.
    let (owner, join) = start_owner(&catalog);
    let client = owner.register();
    let call = |method: &str, params: Value| {
        let response = owner
            .call(
                client,
                ApiRequest {
                    id: method.into(),
                    method: method.into(),
                    params,
                    token: None,
                },
            )
            .expect("the owner answered");
        match response.error {
            None => response.result.expect("a result"),
            Some(error) => panic!("{method}: {}", serde_json::to_value(error).unwrap()),
        }
    };
    let settled = |job: &Value| {
        let read = luxforge_testbase::wait_for("the job to settle", || {
            let read = call("job.read", json!({"job_id": job["job_id"]}));
            (!matches!(read["status"].as_str(), Some("queued" | "running"))).then_some(read)
        });
        assert_eq!(read["status"], "ready", "{read}");
        read["result"].clone()
    };
    let plan = call(
        "merge.plan",
        json!({"kind": "panorama", "targets": {"kind": "paths", "paths": paths}}),
    );
    assert_eq!(plan["suitable"], true, "{plan}");
    assert_eq!(plan["provider"]["available"], true, "{plan}");
    let created = call(
        "merge.create",
        json!({"kind": "panorama", "targets": {"kind": "paths", "paths": paths},
               "plan_id": plan["plan_id"], "mutation": envelope("create")}),
    );
    let result = settled(&created);
    let asset = result["result_asset_id"].clone();
    settled(&call("source.prepare", json!({"asset_id": asset})));
    let inspected = call("merge.inspect", json!({"merge_id": result["merge_id"]}));
    let provenance = &inspected["provenance"];
    let resolved = &provenance["resolved"]["panorama"];
    let stitch = &resolved["stitch"];
    let id = AssetId::parse(asset.as_str().unwrap().to_owned()).unwrap();
    let (answer, answered) = std::sync::mpsc::channel();
    owner
        .sender
        .send(OwnerMessage::Catalog(CatalogMessage::Merge(
            LibraryMessage::Run(Box::new(move |owner| {
                let _ = answer.send(owner.service.derived_source_read(&id).unwrap());
            })),
        )))
        .unwrap();
    let read = answered.recv().unwrap();
    let image = crate::artifacts::derived_source::read_derived_source(
        &read,
        &std::sync::atomic::AtomicBool::new(false),
    )
    .unwrap();
    owner.stop();
    join.join().unwrap();

    // The registered geometry, as the provenance records it, in the stack's order.
    let number = |value: &Value| value.as_f64().unwrap();
    let lens = &resolved["lens"];
    let registered_lens = reference::Lens {
        width: lens["width"].as_u64().unwrap() as usize,
        height: lens["height"].as_u64().unwrap() as usize,
        focal_px: number(&lens["focal_px"]),
        radial: number(&lens["radial"]),
    };
    let rotations: Vec<reference::Mat3> =
        serde_json::from_value(resolved["rotations"].clone()).unwrap();
    let registered: Vec<Pose> = rotations
        .iter()
        .map(|rotation| {
            let m = reference::transpose(rotation);
            Pose::new(
                m[0][2].atan2(m[2][2]).to_degrees(),
                (-m[1][2]).asin().to_degrees(),
                m[1][0].atan2(m[1][1]).to_degrees(),
            )
        })
        .collect();
    let at = resolved["reference"].as_u64().unwrap() as usize;
    let projection = match stitch["projection"].as_str().unwrap() {
        "perspective" => reference::Projection::Rectilinear,
        "cylindrical" => reference::Projection::Cylindrical,
        _ => reference::Projection::Equirectangular,
    };
    let layout = reference::stitch_layout(
        projection,
        &registered_lens,
        &registered,
        at,
        number(&stitch["scale"]),
    );
    assert_eq!(
        (layout.width as u64, layout.height as u64),
        (
            stitch["bounds_width"].as_u64().unwrap(),
            stitch["bounds_height"].as_u64().unwrap()
        ),
        "{stitch}"
    );
    let crop = &stitch["crop"];
    let (x0, y0) = (crop["x"].as_u64().unwrap(), crop["y"].as_u64().unwrap());
    assert_eq!(
        (u64::from(image.width()), u64::from(image.height())),
        (
            crop["width"].as_u64().unwrap(),
            crop["height"].as_u64().unwrap()
        )
    );
    // Each frame's true orientation, in the stack's order.
    let truths: Vec<Pose> = (0..registered.len())
        .map(|k| {
            let file = provenance["inputs"][k]["path"].as_str().unwrap();
            poses[paths
                .iter()
                .position(|path| path.to_str().unwrap() == file)
                .unwrap()]
        })
        .collect();
    let scene = Scene::new(SEED, CELL);
    let (width, height) = (image.width() as usize, image.height() as usize);
    let planes = image.planes();
    let plane = width * height;
    let (mut sum, mut outliers, mut count) = (0.0, 0_usize, 0_usize);
    for y in 0..height {
        for x in 0..width {
            let d = layout.direction((x0 as usize + x) as f64, (y0 as usize + y) as f64);
            // What each frame recording this direction holds there: the scene along the true ray
            // of that frame pixel, at the frames' gain, in 8-bit sRGB.
            let mut low = [f64::INFINITY; 3];
            let mut high = [f64::NEG_INFINITY; 3];
            for (k, pose) in registered.iter().enumerate() {
                let Some(q) = reference::stitch_frame_position(&registered_lens, pose, d) else {
                    continue;
                };
                if !reference::stitch_records(&registered_lens, pose, d) {
                    continue;
                }
                let ray = reference::apply(&truths[k].camera_to_world(), self::lens().ray(q));
                let value = scene.radiance(ray);
                for c in 0..3 {
                    let t = in_jpeg(value[c] * GAIN);
                    low[c] = low[c].min(t);
                    high[c] = high[c].max(t);
                }
            }
            assert!(low[0].is_finite(), "({x}, {y}) is covered");
            let mut worst = 0.0_f64;
            for c in 0..3 {
                let o = f64::from(planes[c * plane + y * width + x]);
                let e = (low[c] - o).max(o - high[c]).max(0.0) / (low[c] + STITCHED_FLOOR);
                sum += e;
                worst = worst.max(e);
            }
            outliers += usize::from(worst > STITCHED_PIXEL);
            count += 1;
        }
    }
    let mean = sum / (3 * count) as f64;
    let outlying = outliers as f64 / count as f64;
    eprintln!(
        "{width} × {height} {}: mean {mean:.5}, outlying {outlying:.4}",
        stitch["projection"]
    );
    assert!(mean <= STITCHED_MEAN, "mean {mean}");
    assert!(outlying <= STITCHED_OUTLIERS, "outlying {outlying}");
    let _ = fs::remove_dir_all(&directory);
}

/// The linear value an 8-bit sRGB code of `linear` decodes to.
fn in_jpeg(linear: f64) -> f64 {
    luxforge_reference::srgb::decode(luxforge_reference::srgb::code(linear))
}
