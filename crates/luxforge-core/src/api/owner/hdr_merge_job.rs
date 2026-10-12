//! The HDR merge job through the real catalog owner and the shared registry, with several clients:
//! planning, creating, cancelling at every stage, inspecting, reusing, rerunning and retrying;
//! queue pressure, slow reads, inputs that change or vanish and a client that disconnects; a
//! restart mid-merge (interrupted), the temporary-storage ceiling, a full disk and a corrupt chunk
//! found after reopening; publication failing at each boundary; the originals' bytes and an
//! unrelated photograph's recipe unchanged, the job's accounting, orphan cleanup and an editor
//! that never waits on a merge, nor a merge on it.
//!
//! No decodable RAW bracket is committed, so the lane's development is a synthetic camera
//! (`luxforge_reference::hdr` and `hdr_align`): each input file is real and read once, its
//! SHA-256 is its identity, and its frame is a textured scene recorded at the file's exposure and
//! small rigid motion. Everything after the development — spooling, analysis, alignment, fusion,
//! durable storage and publication — is the production path.
use super::*;
use crate::{
    LinearImage, ModuleRegistry,
    api::{
        ApiRequest, OwnerHandle,
        owner::{OwnerMessage, catalog::CatalogMessage, library::LibraryMessage},
    },
    artifacts::derived_source::{Step, source_object_path},
    catalog_types::{CameraBody, Dimensions, Exposure},
    editor::{distinct_jpeg, mutation},
    merge::{
        hdr::{SensorClipModel, SensorLayout},
        job::{Developed, Development, Stage},
    },
};
use luxforge_reference::{
    hdr::{CAMERA_TO_SRGB, Camera},
    hdr_align::{self as geometry, Rigid},
};
use luxforge_testbase::Gate;
use std::{
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    thread::JoinHandle,
};

const W: usize = 384;
const H: usize = 256;
/// Each bracket's exposures, in stops from its middle frame, and the rigid motion each was
/// recorded with.
const OFFSETS: [f64; 3] = [-2.0, 0.0, 2.0];
const MOTIONS: [(f64, f64); 3] = [(1.25, -0.75), (0.0, 0.0), (-0.9, 1.1)];

/// One synthetic input: its bracket's scene and its exposure and motion.
#[derive(Clone, Copy)]
struct Shot {
    seed: u64,
    position: usize,
}

/// The synthetic camera the lane develops with. It reads each file once, and holds at `reading`
/// before it does while the gate is shut, giving up when the job is cancelled.
struct Synthetic {
    shots: HashMap<PathBuf, Shot>,
    reading: Arc<Gate>,
    reads: AtomicUsize,
    frames: Mutex<HashMap<PathBuf, Arc<Vec<f32>>>>,
    /// Record a dark disc at another place in every frame, a subject that moved, for the frames
    /// developed from now on.
    moving: std::sync::atomic::AtomicBool,
    /// Record a scene with no texture at all, which no alignment can fix, for the frames
    /// developed from now on.
    flat: std::sync::atomic::AtomicBool,
}

fn camera() -> Camera {
    Camera {
        camera_to_srgb: CAMERA_TO_SRGB,
        gains: [2.0, 1.0, 1.5],
        bayer: true,
    }
}

impl Synthetic {
    fn planes(&self, path: &Path, shot: Shot) -> Arc<Vec<f32>> {
        if let Some(planes) = self.frames.lock().unwrap().get(path) {
            return planes.clone();
        }
        let scene = geometry::textured_scene(shot.seed);
        let scene: Box<dyn Fn(f64, f64) -> [f64; 3]> = if self.flat.load(Ordering::SeqCst) {
            Box::new(|_, _| [0.18; 3])
        } else if self.moving.load(Ordering::SeqCst) {
            let centre = [(120.0, 100.0), (190.0, 128.0), (260.0, 150.0)][shot.position];
            Box::new(geometry::with_disc(scene, centre, 18.0, [0.004; 3]))
        } else {
            Box::new(scene)
        };
        let (dx, dy) = MOTIONS[shot.position];
        let exposure = geometry::record(
            &scene,
            W,
            H,
            &Rigid::new(W, H, 0.0, (dx, dy)),
            &camera(),
            OFFSETS[shot.position],
            None,
        );
        let mut planes = Vec::with_capacity(3 * W * H);
        for plane in &exposure.rgb {
            planes.extend_from_slice(plane);
        }
        let planes = Arc::new(planes);
        self.frames
            .lock()
            .unwrap()
            .insert(path.to_path_buf(), planes.clone());
        planes
    }
}

impl Developer for Synthetic {
    fn develop(
        &self,
        path: &Path,
        _gains: Option<[f32; 3]>,
        control: &JobControl,
    ) -> Result<Developed, Error> {
        if !self.reading.pass_unless(|| control.is_cancelled()) {
            return Err(control.cancelled_error());
        }
        control.checkpoint()?;
        let bytes = fs::read(path)
            .map_err(|error| crate::atomic_file::file_error(path.display(), error.kind()))?;
        self.reads.fetch_add(1, Ordering::SeqCst);
        let fingerprint = format!("{:x}", Sha256::digest(&bytes));
        let shot = *self
            .shots
            .get(path)
            .ok_or_else(|| Error::unsupported_input(format!("{} is no frame", path.display())))?;
        let header = HeaderMetadata {
            camera: Some(CameraBody {
                make: "NIKON CORPORATION".into(),
                model: "NIKON Z 6".into(),
                serial: None,
            }),
            lens: Some("NIKKOR Z 24-70mm f/4 S".into()),
            exposure: Exposure {
                time_s: Some((OFFSETS[shot.position].exp2() / 250.0) as f32),
                f_number: Some(8.0),
                iso: Some(100),
                bias_ev: None,
                focal_mm: Some(24.0),
                focal_35mm_mm: None,
            },
            dimensions: Some(Dimensions {
                width: W as u32,
                height: H as u32,
            }),
            ..HeaderMetadata::default()
        };
        let image = LinearImage::new(W as u32, H as u32, (*self.planes(path, shot)).clone())?;
        let rgb_cam = CAMERA_TO_SRGB.map(|row| row.map(|value| value as f32));
        Ok(Developed {
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
            development: Some(Development {
                gains: [2.0, 1.0, 1.5],
                cam_xyz: [
                    [0.92, -0.31, -0.05],
                    [-0.45, 1.25, 0.2],
                    [-0.05, 0.15, 0.6],
                    [0.0, 0.0, 0.0],
                ],
                image,
                clip: SensorClipModel::new(rgb_cam, [2.0, 1.0, 1.5], SensorLayout::Bayer)?,
            }),
        })
    }
}

/// A catalog beside brackets of synthetic RAW files, an ordinary photograph with an edit of its
/// own, a folder for the results, and the owner running with the synthetic camera.
struct Bench {
    directory: PathBuf,
    catalog: PathBuf,
    /// Three files a bracket, in order.
    files: Vec<PathBuf>,
    hashes: Vec<String>,
    photograph: AssetId,
    developer: Arc<Synthetic>,
    owner: OwnerHandle,
    join: Option<JoinHandle<()>>,
    client: ClientId,
    folder: Value,
    /// The catalog's artifact root.
    root: PathBuf,
    source_hold: Option<Arc<dyn Fn() + Send + Sync>>,
}

fn sha256(path: &Path) -> String {
    format!("{:x}", Sha256::digest(fs::read(path).unwrap()))
}

impl Bench {
    fn new(name: &str, brackets: usize) -> Self {
        Self::with(name, brackets, None)
    }

    fn with(name: &str, brackets: usize, source_hold: Option<Arc<dyn Fn() + Send + Sync>>) -> Self {
        let directory = luxforge_testbase::paths::temp_dir(&format!("hdr-merge-job-{name}"))
            .canonicalize()
            .unwrap();
        let folder = directory.join("bracket");
        fs::create_dir_all(&folder).unwrap();
        let mut files = Vec::new();
        let mut shots = HashMap::new();
        for bracket in 0..brackets {
            for position in 0..3 {
                let path = folder.join(format!("DSC_{:04}.NEF", 3 * bracket + position + 1));
                // Distinct bytes, a little over one read buffer, so each file is its own frame.
                let mut bytes =
                    format!("synthetic bracket {bracket} frame {position}\n").into_bytes();
                bytes.resize(1 << 16, (bracket * 3 + position) as u8);
                fs::write(&path, bytes).unwrap();
                shots.insert(
                    path.clone(),
                    Shot {
                        seed: 11 + bracket as u64,
                        position,
                    },
                );
                files.push(path);
            }
        }
        let hashes = files.iter().map(|path| sha256(path)).collect();
        let catalog = directory.join("catalog.sqlite");
        let photograph = {
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
            state.asset.id
        };
        let developer = Arc::new(Synthetic {
            shots,
            reading: Arc::new(Gate::new()),
            reads: AtomicUsize::new(0),
            frames: Mutex::new(HashMap::new()),
            moving: std::sync::atomic::AtomicBool::new(false),
            flat: std::sync::atomic::AtomicBool::new(false),
        });
        let (owner, join) = start_owner(&catalog, source_hold.clone());
        let client = owner.register();
        let mut bench = Self {
            directory,
            catalog,
            files,
            hashes,
            photograph,
            developer,
            owner,
            join: Some(join),
            client,
            folder: Value::Null,
            root: PathBuf::new(),
            source_hold,
        };
        bench.install_developer();
        bench.root = bench.on_owner(|owner| owner.service.artifact_root().to_path_buf());
        let created = bench.ok(
            "folder.create",
            json!({"name": "Merges", "mutation": envelope("folder")}),
        );
        bench.folder = created["folder"]["id"].clone();
        bench
    }

    fn install_developer(&self) {
        let developer = self.developer.clone();
        self.lane(move |lane| lane.developer = developer);
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

    /// Stop the owner as Luxforge stops, and start it again on the same catalog.
    fn restart(&mut self) {
        self.owner.stop();
        self.join.take().unwrap().join().unwrap();
        let (owner, join) = start_owner(&self.catalog, self.source_hold.clone());
        self.owner = owner;
        self.join = Some(join);
        self.client = self.owner.register();
        self.install_developer();
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

    fn paths(&self, bracket: usize) -> Vec<&PathBuf> {
        self.files[3 * bracket..3 * bracket + 3].iter().collect()
    }

    fn create_params(&self, bracket: usize, request: &str) -> Value {
        json!({
            "kind": "hdr",
            "targets": {"kind": "paths", "paths": self.paths(bracket)},
            "folder_id": self.folder,
            "mutation": envelope(request),
        })
    }

    /// `merge.create` of one bracket: its job.
    fn create(&self, bracket: usize, request: &str) -> Value {
        self.ok("merge.create", self.create_params(bracket, request))
    }

    fn job(&self, job_id: &Value) -> Value {
        self.ok("job.read", json!({"job_id": job_id}))
    }

    fn settled(&self, job_id: &Value) -> Value {
        luxforge_testbase::wait_for("the merge to settle", || {
            let read = self.job(job_id);
            (!matches!(read["status"].as_str(), Some("queued" | "running"))).then_some(read)
        })
    }

    /// Create a merge of one bracket and wait for its result.
    fn merged(&self, bracket: usize, request: &str) -> Value {
        let job = self.create(bracket, request);
        let read = self.settled(&job["job_id"]);
        assert_eq!(read["status"], "ready", "{read}");
        read["result"].clone()
    }

    fn cancel(&self, job_id: &Value) {
        self.ok("job.cancel", json!({"job_id": job_id}));
    }

    fn reads(&self) -> usize {
        self.developer.reads.load(Ordering::SeqCst)
    }

    /// Run `read` on the owner, in order with every call before it, and answer what it answers:
    /// the owner holds the catalog exclusively.
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

    /// Read the catalog's rows on the owner.
    fn database<T: Send + 'static>(
        &self,
        read: impl FnOnce(&rusqlite::Connection) -> rusqlite::Result<T> + Send + 'static,
    ) -> T {
        self.on_owner(move |owner| read(&owner.service.connection))
            .unwrap()
    }

    fn count(&self, sql: &str) -> i64 {
        let sql = sql.to_owned();
        self.database(move |database| database.query_row(&sql, [], |row| row.get(0)))
    }

    /// The merges folder under the catalog's artifact root.
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

    /// The originals' bytes, the unrelated photograph's row, recipe and history: what no merge
    /// may change.
    fn untouched(&self) -> Vec<Vec<String>> {
        let photograph = self.photograph.clone();
        let mut rows = self.database(move |database| {
            let mut rows = Vec::new();
            for sql in [
                "SELECT * FROM assets WHERE id = ?1",
                "SELECT * FROM asset_state WHERE asset_id = ?1",
                "SELECT * FROM entries WHERE asset_id = ?1 ORDER BY sequence",
            ] {
                let mut statement = database.prepare(sql)?;
                let columns = statement.column_count();
                let read = statement
                    .query_map([photograph.as_str()], |row| {
                        (0..columns)
                            .map(|at| Ok(format!("{:?}", row.get_ref(at)?)))
                            .collect::<Result<Vec<String>, rusqlite::Error>>()
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                rows.extend(read);
            }
            Ok(rows)
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
        self.developer.reading.open();
        self.owner.stop();
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
        let _ = fs::remove_dir_all(&self.directory);
    }
}

fn start_owner(
    catalog: &Path,
    hold: Option<Arc<dyn Fn() + Send + Sync>>,
) -> (OwnerHandle, JoinHandle<()>) {
    match hold {
        Some(hold) => OwnerHandle::start_holding_sources(catalog, hold).unwrap(),
        None => OwnerHandle::start_with(catalog, Arc::new(ModuleRegistry::builtin())).unwrap(),
    }
}

fn envelope(request: &str) -> Value {
    json!({"request_id": request, "actor": "test"})
}

/// A merge planned, created into an existing folder, read while it runs, inspected, retried and
/// asked for again: one result, named after the reference, with its accounting, and nothing else
/// in the catalog or on disk changed.
#[test]
fn hdr_merge_job_plans_creates_and_reuses_one_result_through_the_registry() {
    let bench = Bench::new("create", 1);
    let before = bench.untouched();
    let paths = bench.paths(0);

    // Planning reads headers only: these files are in no index or catalog, so the exposures wait
    // for a fit and no folder holds them yet.
    let plan = bench.ok(
        "merge.plan",
        json!({"kind": "hdr", "targets": {"kind": "paths", "paths": paths}}),
    );
    assert_eq!(plan["kind"], "hdr");
    assert_eq!(plan["suitable"], true, "{plan}");
    assert_eq!(plan["needs_fit"], true);
    assert_eq!(plan["reference"], 1);
    assert_eq!(plan["inputs"].as_array().unwrap().len(), 3);
    assert_eq!(plan["inputs"][0]["path"], json!(paths[0]));
    assert!(plan.get("folder").is_none());
    assert!(
        plan["folder_reason"]
            .as_str()
            .unwrap()
            .contains("folder.create")
    );
    assert_eq!(plan["provider"]["module"], MergeKind::Hdr.module());
    assert_eq!(plan["provider"]["available"], true);
    assert_eq!(
        plan["estimates"]["temp_ceiling_bytes"],
        json!(MergeKind::Hdr.temp_ceiling())
    );
    assert_eq!(bench.reads(), 0, "planning reads no pixels");

    // Without a folder the request is refused, and nothing starts.
    let refused = bench.failure(
        "merge.create",
        json!({
            "kind": "hdr",
            "targets": {"kind": "paths", "paths": paths},
            "mutation": envelope("no-folder"),
        }),
    );
    assert_eq!(refused["code"], "validation", "{refused}");
    // A plan that no longer matches its options is refused.
    let mut stale = bench.create_params(0, "stale");
    stale["plan_id"] = plan["plan_id"].clone();
    stale["hdr"] = json!({"deghost": "off"});
    assert_eq!(bench.failure("merge.create", stale)["code"], "conflict");

    // Held as it aligns, the job reports its stage and what it holds.
    let gate = Arc::new(Gate::new());
    gate.shut();
    bench.hold_at(Stage::Aligning, gate.clone());
    let mut params = bench.create_params(0, "create");
    params["plan_id"] = plan["plan_id"].clone();
    let started = bench.ok("merge.create", params.clone());
    assert_eq!(started["deduplicated"], false);
    gate.wait_reached(1, "the merge's alignment");
    let running = bench.job(&started["job_id"]);
    assert_eq!(running["status"], "running", "{running}");
    assert_eq!(running["kind"], "hdr-merge");
    assert_eq!(running["result"]["stage"]["stage"], "aligning", "{running}");
    let accounting = &running["result"]["accounting"];
    let pixels = (W * H) as u64;
    assert_eq!(
        accounting["temp_bytes"],
        json!(3 * pixels * job::SPOOL_PIXEL)
    );
    assert_eq!(accounting["development_bytes"], json!(pixels * 12));
    assert_eq!(
        accounting["scratch_limit_bytes"],
        json!(MergeKind::Hdr.scratch_limit())
    );
    assert!(accounting["scratch_peak_bytes"].as_u64().unwrap() <= MergeKind::Hdr.scratch_limit());
    assert_eq!(bench.spools().len(), 3, "one spool file an input");
    // The same request again is the same job, and so is the same merge asked for anew.
    assert_eq!(
        bench.ok("merge.create", params.clone())["job_id"],
        started["job_id"]
    );
    let again = bench.create(0, "create-again");
    assert_eq!(
        (&again["job_id"], &again["deduplicated"]),
        (&started["job_id"], &json!(true))
    );
    gate.open();
    let read = bench.settled(&started["job_id"]);
    assert_eq!(read["status"], "ready", "{read}");
    let result = read["result"].clone();
    assert_eq!(result["file_name"], "DSC_0002 HDR");
    assert_eq!(result["merged"], true);
    assert_eq!(result["inputs"], 3);
    assert_eq!(result["reused"], false);
    assert_eq!(result["catalog_folder_id"], bench.folder);
    let range = result["exposure_range_ev"].as_array().unwrap();
    assert!((range[0].as_f64().unwrap() + 2.0).abs() < 1e-6, "{result}");
    assert!((range[1].as_f64().unwrap() - 2.0).abs() < 1e-6, "{result}");
    assert!(result["change"].is_u64(), "one library change: {result}");
    assert_eq!(result["accounting"]["temp_bytes"], 0, "the spool is gone");
    assert_eq!(bench.reads(), 3, "each input read once");
    assert!(bench.spools().is_empty());
    assert!(
        !bench
            .merges()
            .join(request_dir(Request {
                method: "merge.create",
                actor: "test",
                request_id: "create",
            }))
            .exists(),
        "a published request's marker goes"
    );

    // Inspection, by the merge or by its result.
    let inspected = bench.ok("merge.inspect", json!({"merge_id": result["merge_id"]}));
    assert_eq!(
        bench.ok(
            "merge.inspect",
            json!({"asset_id": result["result_asset_id"]})
        ),
        inspected
    );
    assert_eq!(inspected["state"], "active");
    assert_eq!(inspected["source"]["status"], "available", "{inspected}");
    assert_eq!(inspected["rerunnable"], true);
    assert_eq!(
        inspected["provider"]["version"],
        crate::merge::providers::HDR_VERSION
    );
    assert_eq!(
        inspected["provenance"]["provider"],
        json!({"module": MergeKind::Hdr.module(), "version": crate::merge::providers::HDR_VERSION})
    );
    for input in inspected["inputs"].as_array().unwrap() {
        assert_eq!(input["file"], "available");
    }
    // The result's header is the reference's, with no shutter time of one capture.
    let (width, height, make, lens, shutter, f_number, iso): (
        i64,
        i64,
        String,
        String,
        Option<f64>,
        f64,
        i64,
    ) = {
        let asset = result["result_asset_id"].as_str().unwrap().to_owned();
        bench.database(move |database| {
            database.query_row(
                "SELECT a.width, a.height, c.make, c.lens, c.exposure_time_s, c.f_number, c.iso
             FROM assets a JOIN capture c ON c.asset_row = a.row_id WHERE a.id = ?1",
                [asset],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                    ))
                },
            )
        })
    };
    assert!(width > W as i64 * 9 / 10 && width <= W as i64, "{width}");
    assert!(height > H as i64 * 9 / 10 && height <= H as i64, "{height}");
    assert_eq!(
        (make.as_str(), lens.as_str(), shutter, f_number, iso),
        (
            "NIKON CORPORATION",
            "NIKKOR Z 24-70mm f/4 S",
            None,
            8.0,
            100
        )
    );

    // A retry answers the published merge; a new request for the same merge reuses it, reading
    // nothing.
    let retried = bench.ok("merge.create", params);
    assert_eq!(retried["deduplicated"], true);
    let retried = bench.settled(&retried["job_id"]);
    assert_eq!(retried["result"]["merge_id"], result["merge_id"]);
    let reused = bench.merged(0, "create-once-more");
    assert_eq!(reused["merge_id"], result["merge_id"]);
    assert_eq!(reused["reused"], true);
    assert_eq!(bench.reads(), 3);
    assert_eq!(bench.count("SELECT COUNT(*) FROM merges"), 1);

    // The result is no input of another merge.
    let generated = bench.failure(
        "merge.create",
        json!({
            "kind": "hdr",
            "targets": {"kind": "assets", "asset_ids": [result["result_asset_id"], bench.photograph]},
            "folder_id": bench.folder,
            "mutation": envelope("of-a-result"),
        }),
    );
    assert_eq!(generated["code"], "validation", "{generated}");
    assert!(
        generated["message"]
            .as_str()
            .unwrap()
            .contains("merged photograph")
    );

    assert_eq!(bench.untouched(), before);
    bench.originals_unchanged();
}

/// A merge result has no original: a check never looks for it or records it missing, and Locate,
/// a find and a relink each refuse it by name.
#[test]
fn hdr_merge_job_result_has_no_original_to_check_locate_find_or_relink() {
    let bench = Bench::new("derived-locator", 1);
    let result = bench.merged(0, "create");
    let asset = result["result_asset_id"].clone();

    let check = bench.ok(
        "source.check",
        json!({"targets": {"kind": "assets", "asset_ids": [asset, bench.photograph]}}),
    );
    let check = bench.settled(&check["job_id"]);
    assert_eq!(check["status"], "ready", "{check}");
    let rows = check["result"]["rows"].as_array().unwrap();
    assert_eq!(
        rows.len(),
        1,
        "only the photograph with an original: {check}"
    );
    assert_eq!(rows[0]["asset_id"], json!(bench.photograph));
    assert_eq!(
        bench.count(&format!(
            "SELECT COUNT(*) FROM assets WHERE id = '{}' AND availability IN ('missing', 'offline', 'changed')",
            asset.as_str().unwrap()
        )),
        0
    );
    let missing = bench.ok("source.missing", json!({}));
    assert!(
        missing.to_string().find(asset.as_str().unwrap()).is_none(),
        "{missing}"
    );

    let named = |error: Value, action: &str| {
        assert_eq!(error["code"], "validation", "{error}");
        let message = error["message"].as_str().unwrap();
        assert!(
            message.contains("DSC_0002 HDR") && message.contains(action),
            "{message}"
        );
        assert_eq!(error["data"]["asset_id"], asset);
    };
    named(
        bench.failure(
            "source.locate",
            json!({"asset_id": asset, "path": bench.files[1], "mutation": envelope("locate")}),
        ),
        "locate",
    );
    named(
        bench.failure(
            "source.find",
            json!({"search_root": bench.directory, "targets": {"kind": "assets", "asset_ids": [asset]}}),
        ),
        "look for",
    );
    named(
        bench.failure(
            "source.relink",
            json!({"pairs": [{"asset_id": asset, "path": bench.files[1]}], "mutation": envelope("relink")}),
        ),
        "relink",
    );
    bench.originals_unchanged();
}

/// `job.cancel` stops a merge at every stage, publishes nothing and leaves no spool; once the
/// worker's result reached the owner first, the merge is published and a cancel finds it ready.
#[test]
fn hdr_merge_job_cancels_at_every_stage_and_a_late_cancel_never_misreports() {
    let bench = Bench::new("cancel", 1);
    let before = bench.untouched();

    // While reading: the development itself gives up.
    bench.developer.reading.shut();
    let reading = bench.create(0, "cancel-reading");
    bench.developer.reading.wait_reached(1, "the first read");
    bench.cancel(&reading["job_id"]);
    let read = bench.settled(&reading["job_id"]);
    assert_eq!(read["status"], "cancelled", "{read}");
    bench.developer.reading.open();

    for (at, stage) in [
        Stage::Spooled(1),
        Stage::Reading(2),
        Stage::Aligning,
        Stage::Merging(0),
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
        // Its retry answers how it ended.
        let retry = bench.create(0, &format!("cancel-{at}"));
        assert_eq!(retry["deduplicated"], true);
        assert_eq!(bench.job(&retry["job_id"])["status"], "cancelled");
    }

    // Held after the source is durable, the job's answer reaches the owner before the cancel:
    // published, and the cancel finds it ready.
    bench.lane(|lane| lane.hooks.stage = None);
    let result = bench.merged(0, "published");
    let job = bench.create(0, "published");
    bench.cancel(&job["job_id"]);
    let read = bench.job(&job["job_id"]);
    assert_eq!(read["status"], "ready");
    assert_eq!(read["result"]["merge_id"], result["merge_id"]);
    assert_eq!(bench.count("SELECT COUNT(*) FROM merges"), 1);
    assert_eq!(bench.untouched(), before);
}

/// One merge runs and one waits; a third is refused, not dropped. Neither a client's disconnect
/// nor its other requests cancel a merge, and a slow read never blocks the owner.
#[test]
fn hdr_merge_job_queue_pressure_slow_reads_and_disconnect() {
    let bench = Bench::new("pressure", 3);
    let other = bench.owner.register();
    bench.developer.reading.shut();
    let running = bench.create(0, "first");
    bench
        .developer
        .reading
        .wait_reached(1, "the first merge's read");
    let waiting = bench
        .call(other, "merge.create", bench.create_params(1, "second"))
        .unwrap();
    assert_eq!(bench.job(&waiting["job_id"])["status"], "queued");
    let refused = bench.failure("merge.create", bench.create_params(2, "third"));
    assert_eq!(refused["code"], "resource-limit", "{refused}");
    // While the read is held, the owner answers.
    let plan = bench.ok(
        "merge.plan",
        json!({"kind": "hdr", "targets": {"kind": "paths", "paths": bench.paths(2)}}),
    );
    assert_eq!(plan["suitable"], true);
    // The waiting job's retry is that job.
    assert_eq!(
        bench
            .call(other, "merge.create", bench.create_params(1, "second"))
            .unwrap()["job_id"],
        waiting["job_id"]
    );
    // The client that started the running merge goes; its merge does not.
    bench.owner.disconnect(bench.client);
    bench.developer.reading.open();
    let read = |job: &Value| {
        luxforge_testbase::wait_for("the merge to settle", || {
            let read = bench
                .call(other, "job.read", json!({"job_id": job}))
                .unwrap();
            (!matches!(read["status"].as_str(), Some("queued" | "running"))).then_some(read)
        })
    };
    assert_eq!(read(&running["job_id"])["status"], "ready");
    assert_eq!(read(&waiting["job_id"])["status"], "ready");
    // An input an active stack claims is refused, naming the stack.
    let claimed = bench
        .call(
            other,
            "merge.create",
            json!({
                "kind": "hdr",
                "targets": {"kind": "paths", "paths": [&bench.files[0], &bench.files[6], &bench.files[7]]},
                "folder_id": bench.folder,
                "mutation": envelope("claimed"),
            }),
        )
        .unwrap_err();
    assert_eq!(claimed["code"], "conflict", "{claimed}");
    assert!(claimed["data"]["stack"].is_string());
    assert_eq!(bench.count("SELECT COUNT(*) FROM merges"), 2);
    bench.originals_unchanged();
}

/// An input that changes or goes while the merge runs fails it, naming the file; nothing is
/// published.
#[test]
fn hdr_merge_job_fails_when_an_input_changes_or_vanishes() {
    let bench = Bench::new("inputs", 3);
    // The reference is read first; the first input changes while it is.
    bench.developer.reading.shut();
    let changed = bench.create(0, "changed");
    bench
        .developer
        .reading
        .wait_reached(1, "the reference's read");
    fs::write(&bench.files[0], b"another file now").unwrap();
    bench.developer.reading.open();
    let read = bench.settled(&changed["job_id"]);
    assert_eq!(read["status"], "failed", "{read}");
    assert_eq!(read["error"]["code"], "conflict");
    assert_eq!(read["error"]["data"]["path"], json!(bench.files[0]));

    // An input that goes after it was read fails the check before publication.
    let gate = Arc::new(Gate::new());
    gate.shut();
    bench.hold_at(Stage::Merging(0), gate.clone());
    let vanished = bench.create(1, "vanished");
    gate.wait_reached(1, "the merge");
    fs::remove_file(&bench.files[5]).unwrap();
    gate.open();
    let read = bench.settled(&vanished["job_id"]);
    assert_eq!(read["status"], "failed", "{read}");
    assert_eq!(read["error"]["code"], "source-unavailable");
    assert_eq!(read["error"]["data"]["path"], json!(bench.files[5]));
    assert_eq!(bench.count("SELECT COUNT(*) FROM merges"), 0);
    assert!(bench.spools().is_empty());

    // Requested with an input gone, the merge is refused at once.
    let refused = bench.failure("merge.create", bench.create_params(1, "gone"));
    assert_eq!(refused["code"], "source-unavailable", "{refused}");
    bench.lane(|lane| lane.hooks.stage = None);
    assert_eq!(bench.merged(2, "untouched")["inputs"], 3);
}

/// The temporary-storage ceiling, the disk's free space, a disk that fills while spooling and the
/// scratch limit each refuse the merge with `resource-limit`, leaving no spool; a retry answers
/// the error, and a new request merges once the limit allows.
#[test]
fn hdr_merge_job_refuses_past_the_temp_ceiling_a_full_disk_and_the_scratch_limit() {
    let bench = Bench::new("limits", 1);
    type Limit = Box<dyn Fn(&mut MergeLane) + Send + Sync>;
    let cases: [(&str, Limit, &str); 4] = [
        (
            "ceiling",
            Box::new(|lane| lane.limits_mut(MergeKind::Hdr).temp_ceiling = 1 << 20),
            "ceiling_bytes",
        ),
        (
            "free-space",
            Box::new(|lane| lane.limits_mut(MergeKind::Hdr).available = Some(1 << 20)),
            "available_bytes",
        ),
        (
            "disk-full",
            Box::new(|lane| lane.hooks.spool_full = Some(0)),
            "",
        ),
        (
            "scratch",
            Box::new(|lane| lane.limits_mut(MergeKind::Hdr).scratch = 1 << 20),
            "",
        ),
    ];
    for (name, limit, datum) in cases {
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
        assert!(bench.spools().is_empty(), "{name}");
        let retry = bench.create(0, name);
        assert_eq!(
            bench.job(&retry["job_id"])["error"]["code"],
            "resource-limit"
        );
        bench.lane(|lane| {
            *lane.limits_mut(MergeKind::Hdr) = Limits::of(MergeKind::Hdr);
            lane.hooks = Hooks::default();
        });
    }
    assert_eq!(bench.count("SELECT COUNT(*) FROM merges"), 0);
    // The plan reports the limits a merge is held to.
    bench.lane(|lane| lane.limits_mut(MergeKind::Hdr).available = Some(123));
    let plan = bench.ok(
        "merge.plan",
        json!({"kind": "hdr", "targets": {"kind": "paths", "paths": bench.paths(0)}}),
    );
    assert_eq!(plan["estimates"]["available_bytes"], 123);
    bench.lane(|lane| *lane.limits_mut(MergeKind::Hdr) = Limits::of(MergeKind::Hdr));
    assert_eq!(bench.merged(0, "fits")["inputs"], 3);
    bench.originals_unchanged();
}

/// A failure at each step of writing the result's durable source, or of the publication's
/// transaction, publishes nothing and changes nothing; the next merge publishes.
#[test]
fn hdr_merge_job_publication_failure_at_each_boundary_publishes_nothing() {
    let bench = Bench::new("publication", 1);
    let before = bench.untouched();
    let assets = bench.count("SELECT COUNT(*) FROM assets");
    let changes = bench.count("SELECT COUNT(*) FROM library_changes");
    for (at, step) in [
        Step::StageChunk(0),
        Step::RenameChunk(0),
        Step::StageManifest,
        Step::RenameManifest,
        Step::SyncDirectory,
    ]
    .into_iter()
    .enumerate()
    {
        bench.lane(move |lane| lane.hooks.writer_fails = Some(step));
        let started = bench.create(0, &format!("writer-{at}"));
        let read = bench.settled(&started["job_id"]);
        assert_eq!(read["status"], "failed", "{step:?}: {read}");
        assert_eq!(bench.count("SELECT COUNT(*) FROM merges"), 0, "{step:?}");
        assert_eq!(
            bench.count("SELECT COUNT(*) FROM assets"),
            assets,
            "{step:?}"
        );
        assert!(bench.spools().is_empty(), "{step:?}");
    }
    bench.lane(|lane| {
        lane.hooks = Hooks::default();
        lane.publish_fails = Some(Error::new(ErrorKind::Catalog, "the commit was refused"));
    });
    let started = bench.create(0, "transaction");
    let read = bench.settled(&started["job_id"]);
    assert_eq!(read["status"], "failed", "{read}");
    assert_eq!(read["error"]["code"], "catalog");
    assert_eq!(bench.count("SELECT COUNT(*) FROM merges"), 0);
    assert_eq!(bench.count("SELECT COUNT(*) FROM assets"), assets);
    assert_eq!(
        bench.count("SELECT COUNT(*) FROM library_changes"),
        changes,
        "no library change"
    );
    // Its retry answers the error; a new request publishes.
    let retry = bench.create(0, "transaction");
    assert_eq!(bench.job(&retry["job_id"])["error"]["code"], "catalog");
    assert_eq!(bench.merged(0, "after")["inputs"], 3);
    assert_eq!(bench.count("SELECT COUNT(*) FROM assets"), assets + 1);
    assert_eq!(bench.untouched(), before);
}

/// Pick merged, the development set and the lane, through the registry.
impl Bench {
    /// `merge.create` as Pick merged, naming the set the client believes is current when `set` is.
    fn pick_params(&self, bracket: usize, request: &str, set: Option<&Value>) -> Value {
        let mut params = self.create_params(bracket, request);
        params["add_to_development_set"] = json!(true);
        if let Some(set) = set {
            params["set_id"] = set.clone();
        }
        params
    }

    /// Pick merged of one bracket and its result.
    fn picked(&self, bracket: usize, request: &str, set: Option<&Value>) -> Value {
        let job = self.ok("merge.create", self.pick_params(bracket, request, set));
        let read = self.settled(&job["job_id"]);
        assert_eq!(read["status"], "ready", "{read}");
        read["result"].clone()
    }

    fn set(&self) -> Value {
        self.ok("development-set.read", json!({}))
    }

    /// The current set's members, in order.
    fn members(&self) -> Vec<Value> {
        self.set()["members"]
            .as_array()
            .unwrap()
            .iter()
            .map(|member| member["asset_id"].clone())
            .collect()
    }

    /// Replace the current set with one of `member`, or with none, answering the new set's identity.
    fn replace_set(&self, member: Option<&Value>, request: &str) -> Value {
        let source = match member {
            Some(member) => json!({"kind": "assets", "asset_ids": [member]}),
            None => json!({"kind": "none"}),
        };
        let replaced = self.ok(
            "development-set.replace",
            json!({"source": source, "mutation": envelope(request)}),
        );
        replaced["set_id"].clone()
    }

    fn as_actor(&self, request: &str, actor: &str) -> Value {
        json!({"request_id": request, "actor": actor})
    }
}

/// Pick merged appends its result to the current set once, in the change that publishes it, keeping
/// the members and their order, and starts a one-result set when there is none; Import merged
/// leaves the set alone; Pick merged of an exact existing result appends it in a change of its own,
/// once, and a retry is answered as first.
#[test]
fn hdr_merge_job_pick_merged_appends_the_result_once_and_import_leaves_the_set() {
    let bench = Bench::new("pick", 3);
    let before = bench.untouched();
    let photograph = json!(bench.photograph);

    // With no set, Pick merged starts a set of the one result.
    assert_eq!(bench.set()["count"], 0);
    let first = bench.picked(0, "pick-no-set", None);
    assert_eq!(first["reused"], false);
    assert!(first["change"].is_u64(), "{first}");
    let read = bench.set();
    assert_eq!(read["count"], 1);
    assert_eq!(read["members"][0]["asset_id"], first["result_asset_id"]);
    assert!(read["set_id"].is_string());

    // Import merged leaves the person's set as it is.
    let set = bench.replace_set(Some(&photograph), "open");
    let revision = bench.set()["revision"].clone();
    let imported = bench.merged(1, "import");
    assert_eq!(bench.members(), std::slice::from_ref(&photograph));
    assert_eq!(bench.set()["revision"], revision, "Import changes no set");

    // Pick merged appends after the members, in the same change that publishes the result.
    let picked = bench.picked(2, "pick", Some(&set));
    assert_eq!(picked["reused"], false);
    assert!(picked["change"].is_u64());
    let read = bench.set();
    assert_eq!(read["set_id"], set, "the same set");
    assert!(read["revision"].as_u64() > revision.as_u64());
    assert_eq!(
        bench.members(),
        [photograph.clone(), picked["result_asset_id"].clone()]
    );
    assert_eq!(
        bench.count(
            "SELECT COUNT(*) FROM library_changes c
             WHERE EXISTS (SELECT 1 FROM library_change_rows r
                 WHERE r.change_seq = c.sequence AND r.item_kind = 'merge-stack')
             AND EXISTS (SELECT 1 FROM library_change_rows r
                 WHERE r.change_seq = c.sequence AND r.item_kind = 'set-member')"
        ),
        2,
        "each Pick that published recorded its membership in the change that made the result"
    );

    // An exact active result is not merged again; Pick appends it once.
    let reads = bench.reads();
    let existing = bench.picked(1, "pick-existing", Some(&set));
    assert_eq!(existing["reused"], true);
    assert_eq!(existing["merge_id"], imported["merge_id"]);
    assert!(existing["change"].is_u64(), "{existing}");
    let members = [
        photograph.clone(),
        picked["result_asset_id"].clone(),
        imported["result_asset_id"].clone(),
    ];
    assert_eq!(bench.members(), members);
    let revision = bench.set()["revision"].clone();
    // A second Pick of it, and an Import, change nothing.
    let again = bench.picked(1, "pick-existing-again", Some(&set));
    assert_eq!(again["reused"], true);
    assert!(again["change"].is_null(), "{again}");
    assert_eq!(bench.members(), members);
    assert_eq!(bench.set()["revision"], revision);
    assert!(bench.merged(1, "import-existing")["change"].is_null());
    // A retry of the first is that answer, not a second append.
    let retried = bench.picked(1, "pick-existing", Some(&set));
    assert_eq!(retried["reused"], true);
    assert_eq!(retried["change"], existing["change"]);
    assert_eq!(bench.members(), members);

    assert_eq!(bench.count("SELECT COUNT(*) FROM merges"), 3);
    assert_eq!(bench.reads(), reads, "reuse reads no input");
    assert_eq!(bench.untouched(), before);
    bench.originals_unchanged();
}

/// A `set_id` that is not the current set is a conflict at admission, naming both sets, and starts
/// nothing, for a new merge and for an exact existing result; without `add_to_development_set` it
/// is not looked at.
#[test]
fn hdr_merge_job_pick_merged_names_a_set_that_is_not_current_as_a_conflict() {
    let bench = Bench::new("pick-stale", 2);
    let photograph = json!(bench.photograph);
    bench.merged(0, "import");
    let stale = bench.replace_set(Some(&photograph), "open-1");
    let current = bench.replace_set(Some(&photograph), "open-2");
    let reads = bench.reads();

    let refused = bench.failure(
        "merge.create",
        bench.pick_params(1, "stale-new", Some(&stale)),
    );
    assert_eq!(refused["code"], "conflict", "{refused}");
    assert!(
        refused["message"]
            .as_str()
            .unwrap()
            .contains(stale.as_str().unwrap()),
        "{refused}"
    );
    assert_eq!(refused["data"]["current"], current);
    let refused = bench.failure(
        "merge.create",
        bench.pick_params(0, "stale-existing", Some(&stale)),
    );
    assert_eq!(refused["code"], "conflict", "{refused}");
    // With no set at all, any named set is gone.
    bench.replace_set(None, "close");
    let refused = bench.failure(
        "merge.create",
        bench.pick_params(1, "no-set", Some(&current)),
    );
    assert_eq!(refused["code"], "conflict", "{refused}");
    assert!(refused["data"]["current"].is_null());
    assert_eq!(bench.count("SELECT COUNT(*) FROM merges"), 1);
    assert_eq!(bench.reads(), reads, "nothing started");
    assert_eq!(bench.set()["count"], 0, "no set was made");

    // Import merged takes no set: a named one is not looked at.
    let mut params = bench.create_params(1, "import-with-set");
    params["set_id"] = stale;
    let job = bench.ok("merge.create", params);
    assert_eq!(bench.settled(&job["job_id"])["status"], "ready");
    assert_eq!(bench.set()["count"], 0);
    bench.originals_unchanged();
}

/// A set replaced, or opened where there was none, while the merge runs fails the whole publication
/// with a named conflict: no result, stack or membership is visible, the other set is as it was,
/// and a new request against the current set publishes.
#[test]
fn hdr_merge_job_pick_merged_fails_whole_when_the_set_is_replaced_while_it_runs() {
    let bench = Bench::new("pick-replaced", 2);
    let before = bench.untouched();
    let photograph = json!(bench.photograph);
    let assets = bench.count("SELECT COUNT(*) FROM assets");
    let changes = bench.count("SELECT COUNT(*) FROM library_changes");
    let members = bench.count("SELECT COUNT(*) FROM development_set_members");

    // The set the request was made against is replaced.
    let original = bench.replace_set(Some(&photograph), "open-1");
    let gate = Arc::new(Gate::new());
    gate.shut();
    bench.hold_at(Stage::Aligning, gate.clone());
    let started = bench.ok(
        "merge.create",
        bench.pick_params(0, "pick-a", Some(&original)),
    );
    gate.wait_reached(1, "the merge's alignment");
    let replacement = bench.replace_set(Some(&photograph), "open-2");
    gate.open();
    let read = bench.settled(&started["job_id"]);
    assert_eq!(read["status"], "failed", "{read}");
    assert_eq!(read["error"]["code"], "conflict", "{read}");
    assert!(
        read["error"]["message"]
            .as_str()
            .unwrap()
            .contains(original.as_str().unwrap()),
        "{read}"
    );
    assert_eq!(read["error"]["data"]["current"], replacement);

    // A set opened where there was none.
    bench.replace_set(None, "close");
    let gate = Arc::new(Gate::new());
    gate.shut();
    bench.hold_at(Stage::Aligning, gate.clone());
    let started = bench.ok("merge.create", bench.pick_params(0, "pick-b", None));
    gate.wait_reached(1, "the merge's alignment");
    let opened = bench.replace_set(Some(&photograph), "open-3");
    gate.open();
    let read = bench.settled(&started["job_id"]);
    assert_eq!(read["status"], "failed", "{read}");
    assert_eq!(read["error"]["code"], "conflict", "{read}");
    assert_eq!(read["error"]["data"]["current"], opened);

    assert_eq!(bench.count("SELECT COUNT(*) FROM merges"), 0);
    assert_eq!(bench.count("SELECT COUNT(*) FROM merge_stacks"), 0);
    assert_eq!(bench.count("SELECT COUNT(*) FROM assets"), assets);
    assert_eq!(bench.count("SELECT COUNT(*) FROM library_changes"), changes);
    assert_eq!(
        bench.count("SELECT COUNT(*) FROM development_set_members"),
        members + 1,
        "only the replacement's own member"
    );
    assert_eq!(bench.set()["set_id"], opened);
    assert_eq!(bench.members(), std::slice::from_ref(&photograph));
    assert!(bench.spools().is_empty());

    // Against the current set, a new request publishes with its member.
    bench.lane(|lane| lane.hooks.stage = None);
    let result = bench.picked(0, "pick-c", Some(&opened));
    assert_eq!(
        bench.members(),
        [photograph, result["result_asset_id"].clone()]
    );
    assert_eq!(bench.count("SELECT COUNT(*) FROM merges"), 1);
    assert_eq!(bench.untouched(), before);
    bench.originals_unchanged();
}

/// `library.undo` of a Pick merge takes the result out of the set and leaves every other member,
/// those another actor added after it too; `library.redo` puts it back at its own place.
#[test]
fn hdr_merge_job_undoing_a_pick_merge_takes_only_its_member_out_of_the_set() {
    let bench = Bench::new("pick-undo", 2);
    let photograph = json!(bench.photograph);
    let imported = bench.merged(0, "import")["result_asset_id"].clone();
    bench.replace_set(Some(&photograph), "open");
    let picked = bench.picked(1, "pick", None);
    let result = picked["result_asset_id"].clone();
    // Another actor adds a member after it.
    bench.ok(
        "development-set.add",
        json!({"asset_ids": [imported], "mutation": bench.as_actor("agent-add", "agent")}),
    );
    let full = [photograph.clone(), result.clone(), imported.clone()];
    assert_eq!(bench.members(), full);

    let undone = bench.ok(
        "library.undo",
        json!({"mutation": bench.as_actor("undo", "test")}),
    );
    assert_eq!(undone["outcome"], "applied", "{undone}");
    assert_eq!(bench.members(), [photograph.clone(), imported.clone()]);
    assert_eq!(
        bench.ok("merge.inspect", json!({"merge_id": picked["merge_id"]}))["state"],
        "undone"
    );
    let redone = bench.ok(
        "library.redo",
        json!({"mutation": bench.as_actor("redo", "test")}),
    );
    assert_eq!(redone["outcome"], "applied", "{redone}");
    assert_eq!(bench.members(), full, "back between the members it was");
    assert_eq!(
        bench.ok("merge.inspect", json!({"merge_id": picked["merge_id"]}))["state"],
        "active"
    );
    assert_eq!(bench.reads(), 6, "undo and redo merge nothing");
    bench.originals_unchanged();
}

/// `merge.rerun` takes the replaced result out of every development set it is a member of, as
/// Remove does, and puts the new result at its place in the current set, in the change that
/// publishes it; `library.undo` puts the replaced result back in each set, at its place, and takes
/// the new one out; `library.redo` makes the replacement again.
#[test]
fn hdr_merge_job_rerun_takes_the_replaced_results_place_in_the_development_set() {
    let bench = Bench::new("rerun-set", 2);
    let photograph = json!(bench.photograph);
    // The old result is a member of a retired set and, between two others, of the current one.
    let old = bench.picked(0, "pick-alone", None);
    let old_result = old["result_asset_id"].clone();
    bench.replace_set(Some(&photograph), "open");
    let other = bench.merged(1, "import")["result_asset_id"].clone();
    bench.ok(
        "development-set.add",
        json!({"asset_ids": [old_result, other], "mutation": envelope("add")}),
    );
    let before = [photograph.clone(), old_result.clone(), other.clone()];
    assert_eq!(bench.members(), before);
    let memberships = |asset: &Value| {
        bench.count(&format!(
            "SELECT COUNT(*) FROM development_set_members m
             JOIN assets a ON a.row_id = m.asset_row WHERE a.id = '{}'",
            asset.as_str().unwrap()
        ))
    };
    assert_eq!(memberships(&old_result), 2);

    let rerun = bench.ok(
        "merge.rerun",
        json!({"merge_id": old["merge_id"], "hdr": {"deghost": "off"}, "mutation": envelope("rerun")}),
    );
    let new = bench.settled(&rerun["job_id"]);
    assert_eq!(new["status"], "ready", "{new}");
    let new = new["result"].clone();
    let new_result = new["result_asset_id"].clone();
    let after = [photograph.clone(), new_result.clone(), other.clone()];
    assert_eq!(
        bench.members(),
        after,
        "the new result in the old one's place"
    );
    assert!(
        bench.set()["members"]
            .as_array()
            .unwrap()
            .iter()
            .all(|member| member.get("removed").is_none()),
        "no removed member is left behind"
    );
    assert_eq!(memberships(&old_result), 0, "out of every set");
    assert_eq!(memberships(&new_result), 1, "only in the current set");
    assert_eq!(
        bench.count(
            "SELECT COUNT(*) FROM library_change_rows WHERE item_kind = 'set-member'
             AND change_seq = (SELECT max(change_seq) FROM library_change_rows)"
        ),
        3,
        "one change: the old result out of both sets and the new one in"
    );

    let undone = bench.ok("library.undo", json!({"mutation": envelope("undo")}));
    assert_eq!(undone["outcome"], "applied", "{undone}");
    assert_eq!(bench.members(), before);
    assert_eq!(memberships(&old_result), 2, "back in both sets");
    assert_eq!(memberships(&new_result), 0);
    let state =
        |merge: &Value| bench.ok("merge.inspect", json!({"merge_id": merge}))["state"].clone();
    assert_eq!(state(&old["merge_id"]), "active");
    assert_eq!(state(&new["merge_id"]), "undone");

    let redone = bench.ok("library.redo", json!({"mutation": envelope("redo")}));
    assert_eq!(redone["outcome"], "applied", "{redone}");
    assert_eq!(bench.members(), after);
    assert_eq!(memberships(&old_result), 0);
    assert_eq!(state(&old["merge_id"]), "removed");
    assert_eq!(state(&new["merge_id"]), "active");
    bench.originals_unchanged();
}

/// Across a restart: a merge the process stopped in answers `interrupted` and publishes nothing,
/// another job sweeps the spool it left, a published merge answers its retries, and a corrupt
/// chunk found after reopening is reported and merged again by a rerun.
#[test]
fn hdr_merge_job_restart_interrupts_sweeps_orphans_and_regenerates_a_corrupt_source() {
    let mut bench = Bench::new("restart", 2);
    bench.developer.reading.shut();
    let interrupted = bench.create(0, "interrupted");
    bench.developer.reading.wait_reached(1, "the read");
    bench.restart();
    bench.developer.reading.open();
    let retry = bench.create(0, "interrupted");
    let read = bench.job(&retry["job_id"]);
    assert_eq!(read["status"], "cancelled", "{read}");
    assert_eq!(read["error"]["data"]["reason"], "interrupted", "{read}");
    assert_ne!(retry["job_id"], interrupted["job_id"]);
    assert_eq!(bench.count("SELECT COUNT(*) FROM merges"), 0);

    // A spool a stopped process left in another request's folder goes when the next job starts;
    // its marker stays.
    let orphan = bench.merges().join("orphan");
    fs::create_dir_all(&orphan).unwrap();
    fs::write(orphan.join(job::spool_name(0)), b"left behind").unwrap();
    fs::write(orphan.join(MARKER), b"{}").unwrap();
    let result = bench.merged(0, "after-restart");
    assert!(!orphan.join(job::spool_name(0)).exists());
    assert!(orphan.join(MARKER).exists());

    // Published, then restarted: the retry answers the merge.
    bench.restart();
    let retry = bench.create(0, "after-restart");
    let read = bench.job(&retry["job_id"]);
    assert_eq!(read["status"], "ready");
    assert_eq!(read["result"]["merge_id"], result["merge_id"]);
    assert_eq!(read["result"]["deduplicated"], true);

    // A chunk of the result's source damaged while Luxforge was stopped.
    let source = result["source_id"].as_str().unwrap().to_owned();
    let chunk: String = bench.database(move |database| {
        database.query_row(
            "SELECT sha256 FROM derived_source_chunks WHERE source_id = ?1 ORDER BY ordinal",
            [&source],
            |row| row.get(0),
        )
    });
    bench.owner.stop();
    bench.join.take().unwrap().join().unwrap();
    let object = source_object_path(&bench.root, &chunk);
    let mut bytes = fs::read(&object).unwrap();
    bytes.push(0);
    fs::write(&object, bytes).unwrap();
    let (owner, join) = start_owner(&bench.catalog, None);
    bench.owner = owner;
    bench.join = Some(join);
    bench.client = bench.owner.register();
    bench.install_developer();
    let inspected = bench.ok("merge.inspect", json!({"merge_id": result["merge_id"]}));
    assert_eq!(inspected["source"]["status"], "corrupt", "{inspected}");
    assert_eq!(inspected["rerunnable"], true);
    let reads = bench.reads();
    let rerun = bench.ok(
        "merge.rerun",
        json!({"merge_id": result["merge_id"], "mutation": envelope("regenerate")}),
    );
    let rerun = bench.settled(&rerun["job_id"]);
    assert_eq!(rerun["status"], "ready", "{rerun}");
    assert_eq!(bench.reads(), reads + 3, "merged again");
    assert_eq!(rerun["result"]["replaced"], result["merge_id"]);
    let renewed = bench.ok(
        "merge.inspect",
        json!({"merge_id": rerun["result"]["merge_id"]}),
    );
    assert_eq!(renewed["source"]["status"], "available");
    assert_eq!(
        bench.ok("merge.inspect", json!({"merge_id": result["merge_id"]}))["state"],
        "removed"
    );
    bench.originals_unchanged();
}

/// A rerun replaces its merge's result in one change; an exact rerun of a whole source merges
/// nothing; Remove and Put back; and a missing or different provider disables create and rerun.
#[test]
fn hdr_merge_job_reruns_removes_restores_and_follows_the_provider() {
    let bench = Bench::new("rerun", 1);
    let first = bench.merged(0, "create");
    let reads = bench.reads();
    let rerun = bench.ok(
        "merge.rerun",
        json!({"merge_id": first["merge_id"], "hdr": {"deghost": "off"}, "mutation": envelope("rerun")}),
    );
    let second = bench.settled(&rerun["job_id"]);
    assert_eq!(second["status"], "ready", "{second}");
    let second = second["result"].clone();
    assert_eq!(second["replaced"], first["merge_id"]);
    assert_ne!(second["result_asset_id"], first["result_asset_id"]);
    assert_eq!(bench.reads(), reads + 3);
    let old = bench.ok("merge.inspect", json!({"merge_id": first["merge_id"]}));
    assert_eq!(old["state"], "removed");
    let new = bench.ok("merge.inspect", json!({"merge_id": second["merge_id"]}));
    assert_eq!(
        new["parameters"]["hdr"]["alignment"]["deghost"], "off",
        "{new}"
    );

    // An exact rerun of a whole source gives the new result the same source, merging nothing.
    let exact = bench.ok(
        "merge.rerun",
        json!({"merge_id": second["merge_id"], "mutation": envelope("exact")}),
    );
    let exact = bench.settled(&exact["job_id"]);
    assert_eq!(exact["status"], "ready", "{exact}");
    assert_eq!(exact["result"]["source_id"], second["source_id"]);
    assert_eq!(bench.reads(), reads + 3);
    let third = exact["result"].clone();

    // Remove and Put back; the first merge cannot come back while the third claims its inputs.
    let removed = bench.ok(
        "merge.remove",
        json!({"merge_ids": [third["merge_id"]], "mutation": envelope("remove")}),
    );
    assert_eq!(removed["outcome"], "applied", "{removed}");
    assert_eq!(
        bench.ok("merge.inspect", json!({"merge_id": third["merge_id"]}))["state"],
        "removed"
    );
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

    // Another provider version: reruns are incompatible.
    bench.lane(|lane| {
        lane.set_provider(
            MergeKind::Hdr,
            Ok(MergeProvider {
                module: MergeKind::Hdr.module().into(),
                version: crate::merge::providers::HDR_VERSION + 1,
            }),
        );
    });
    let incompatible = bench.failure(
        "merge.rerun",
        json!({"merge_id": third["merge_id"], "mutation": envelope("newer")}),
    );
    assert_eq!(incompatible["code"], "incompatible", "{incompatible}");
    let inspected = bench.ok("merge.inspect", json!({"merge_id": third["merge_id"]}));
    assert_eq!(inspected["rerunnable"], false);
    assert!(inspected["rerun_reason"].is_string());

    // No provider: nothing is created or rerun, and the plan says why.
    bench.lane(|lane| {
        lane.set_provider(
            MergeKind::Hdr,
            Err("the HDR module is not installed".into()),
        )
    });
    let plan = bench.ok(
        "merge.plan",
        json!({"kind": "hdr", "targets": {"kind": "paths", "paths": bench.paths(0)}}),
    );
    assert_eq!(plan["provider"]["available"], false);
    assert_eq!(
        bench.failure("merge.create", bench.create_params(0, "absent"))["code"],
        "not-ready"
    );
    assert_eq!(
        bench.failure(
            "merge.rerun",
            json!({"merge_id": third["merge_id"], "mutation": envelope("absent")}),
        )["code"],
        "not-ready"
    );

    // A changed input: inspection reports it, and a rerun is refused naming it.
    bench.lane(|lane| {
        lane.set_provider(
            MergeKind::Hdr,
            Ok(crate::merge::providers::provider(MergeKind::Hdr).unwrap()),
        )
    });
    fs::write(&bench.files[2], b"edited elsewhere").unwrap();
    let inspected = bench.ok("merge.inspect", json!({"merge_id": third["merge_id"]}));
    assert_eq!(inspected["inputs"][2]["file"], "changed", "{inspected}");
    assert_eq!(inspected["rerunnable"], false);
    let changed = bench.failure(
        "merge.rerun",
        json!({"merge_id": third["merge_id"], "mutation": envelope("changed")}),
    );
    assert_eq!(changed["code"], "conflict");
    assert_eq!(changed["data"]["path"], json!(bench.files[2]));
}

/// A merge holds no plane lease: an editor prepares and samples a photograph while a merge is
/// held in its development, and a merge runs to its end while the editor's source work is held.
#[test]
fn hdr_merge_job_and_an_editor_never_wait_on_each_other() {
    let sources = Arc::new(Gate::new());
    let hold = sources.clone();
    let bench = Bench::with("editor", 1, Some(Arc::new(move || hold.pass())));
    let sample = json!({"asset_id": bench.photograph, "x": 0, "y": 0});
    let prepared = |bench: &Bench| -> Value {
        for _ in 0..4 {
            match bench.call(bench.client, "render.sample", sample.clone()) {
                Ok(answer) => return answer,
                Err(error) if error["code"] == "preparation-required" => {
                    let job = error["job_id"].clone();
                    let read = bench.settled(&job);
                    assert_eq!(read["status"], "ready", "{read}");
                }
                Err(error) => panic!("render.sample: {error}"),
            }
        }
        panic!("render.sample kept asking for preparation");
    };

    // The editor's preparation is held on the source worker; a merge runs to its end beside it.
    sources.shut();
    let pending = bench
        .call(bench.client, "render.sample", sample.clone())
        .expect_err("the photograph is not prepared yet");
    assert_eq!(pending["code"], "preparation-required", "{pending}");
    sources.wait_reached(1, "the editor's preparation");
    let first = bench.merged(0, "beside-the-editor");
    assert_eq!(
        bench.job(&pending["job_id"])["status"],
        "running",
        "the editor's preparation is still held"
    );
    sources.open();
    assert_eq!(bench.settled(&pending["job_id"])["status"], "ready");

    // A merge is held mid-read; the editor samples its photograph.
    bench.developer.reading.shut();
    let rerun = bench.ok(
        "merge.rerun",
        json!({"merge_id": first["merge_id"], "hdr": {"deghost": "off"}, "mutation": envelope("rerun")}),
    );
    bench.developer.reading.wait_reached(4, "the rerun's read");
    assert!(prepared(&bench)["rgba"].is_array());
    assert_eq!(bench.job(&rerun["job_id"])["status"], "running");
    bench.developer.reading.open();
    assert_eq!(bench.settled(&rerun["job_id"])["status"], "ready");
    bench.originals_unchanged();
}

#[path = "merge_kind_tests.rs"]
mod merge_kind;

#[path = "merge_preview_tests.rs"]
mod merge_preview;
