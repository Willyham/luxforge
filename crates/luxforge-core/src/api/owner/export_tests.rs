//! JPEG export through the catalog owner and the JSON methods: the exported bytes against an
//! independent exact render, the frozen target, retries, both metadata modes, every refusal, the
//! lane's bound and cancellation, and shutdown; and an export streamed through a tile service
//! standing in for the desktop's GPU worker ([`StubTiles`]): its bands encoded in order, the
//! reference option never asking it, a stream it stops drawing rendered again by the reference
//! naming why, and a cancel between its tiles.
use super::*;
use crate::{
    AssetId, Cancel, Evaluation, GpuFallback, SnapshotId,
    api::ApiFailure,
    tiles::{
        Answered, Band, BandStream, TileCall, TileFallback, TileService, TileStatus,
        TileUnavailable,
    },
};
use luxforge_testbase::{
    Gate,
    paths::{jpeg as fixture, temp_dir},
};
use std::{
    fs,
    sync::{Mutex, atomic::AtomicUsize, mpsc},
    time::Duration,
};

/// One owner over a fresh catalog in its own directory, publishing to a board that keeps work of
/// any duration as recent.
struct Harness {
    owner: OwnerHandle,
    join: Option<JoinHandle<()>>,
    client: ClientId,
    dir: PathBuf,
    catalog: PathBuf,
}

impl Harness {
    fn start(name: &str) -> Self {
        let dir = temp_dir(&format!("export-{name}")).canonicalize().unwrap();
        let catalog = dir.join("catalog.sqlite");
        Self::open(dir, catalog, Arc::new(ModuleRegistry::developer()))
    }

    /// [`Self::start`] with the person's preferences kept in `<dir>/config`.
    fn with_preferences(name: &str) -> Self {
        let dir = temp_dir(&format!("export-{name}")).canonicalize().unwrap();
        let catalog = dir.join("catalog.sqlite");
        let (owner, join) = OwnerHandle::start_with_host(
            &catalog,
            Arc::new(ModuleRegistry::developer()),
            HostConfig {
                preferences_dir: Some(dir.join("config")),
                ..HostConfig::unconfigured()
            },
        )
        .unwrap();
        let client = owner.register();
        Self {
            owner,
            join: Some(join),
            client,
            dir,
            catalog,
        }
    }

    /// [`Self::start`] on a host whose GPU provider is `tiles`.
    fn with_tiles(name: &str, tiles: Arc<dyn TileService>) -> Self {
        let dir = temp_dir(&format!("export-{name}")).canonicalize().unwrap();
        let catalog = dir.join("catalog.sqlite");
        let (owner, join) = OwnerHandle::start_with_host(
            &catalog,
            Arc::new(ModuleRegistry::developer()),
            HostConfig {
                tiles: Some(tiles),
                ..HostConfig::unconfigured()
            },
        )
        .unwrap();
        let client = owner.register();
        Self {
            owner,
            join: Some(join),
            client,
            dir,
            catalog,
        }
    }

    fn open(dir: PathBuf, catalog: PathBuf, registry: Arc<ModuleRegistry>) -> Self {
        let (owner, join) = OwnerHandle::start_observed(
            &catalog,
            registry,
            ActivityBoard::with_recent_threshold(Duration::ZERO),
            None,
        )
        .unwrap();
        let client = owner.register();
        Self {
            owner,
            join: Some(join),
            client,
            dir,
            catalog,
        }
    }

    /// The same catalog and directory under a new owner serving `registry`: its source cache is
    /// empty, as after a restart.
    fn reopen(mut self, registry: Arc<ModuleRegistry>) -> Self {
        self.stop();
        let dir = std::mem::take(&mut self.dir);
        Self::open(dir, self.catalog.clone(), registry)
    }

    /// Stop the owner and wait for it, which cancels and joins the export lane.
    fn stop(&mut self) {
        self.owner.stop();
        if let Some(join) = self.join.take() {
            join.join().unwrap();
        }
    }

    fn send(&self, method: &str, params: Value) -> ApiResponse {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let id = format!("{method}-{}", NEXT.fetch_add(1, Ordering::Relaxed));
        self.send_as(&id, method, params)
    }

    fn send_as(&self, id: &str, method: &str, params: Value) -> ApiResponse {
        self.owner
            .call(
                self.client,
                ApiRequest {
                    id: id.into(),
                    method: method.into(),
                    params,
                    token: None,
                },
            )
            .expect("the owner answered")
    }

    fn ok(&self, method: &str, params: Value) -> Value {
        let response = self.send(method, params);
        assert!(response.error.is_none(), "{method}: {:?}", response.error);
        response.result.expect("a result")
    }

    fn refused(&self, method: &str, params: Value) -> ApiFailure {
        self.send(method, params)
            .error
            .unwrap_or_else(|| panic!("{method} was expected to be refused"))
    }

    /// Copy the fixture into this harness's directory, develop it and prepare its photograph, as a
    /// client opens a file; answers the photograph's state.
    fn import(&self, name: &str) -> Value {
        let path = self.dir.join(name);
        fs::copy(fixture(), &path).unwrap();
        self.opened(&path)
    }

    /// Develop the file at `path` and prepare its photograph; answers the photograph's state.
    fn opened(&self, path: &std::path::Path) -> Value {
        let asset = super::library::opening::import(&self.owner, self.client, path);
        self.ok("asset.state", json!({"asset_id": asset}))
    }

    fn settle_source(&self, job: &Value) -> Value {
        luxforge_testbase::wait_for("a source job to settle", || {
            let status = self.ok("job.read", json!({"job_id": job}));
            (!matches!(status["status"].as_str(), Some("queued" | "running"))).then_some(status)
        })
    }

    /// Commit an exposure as the asset's next entry and answer that entry's id.
    fn expose(&self, asset: &Value, exposure: f64) -> Value {
        let revision = self.ok("asset.state", json!({"asset_id": asset}))["revision"].clone();
        self.ok(
            "edit.set-basic",
            json!({
                "asset_id": asset,
                "mutation": {"expected_revision": revision, "request_id": format!("expose-{exposure}-{revision}"), "actor": "test"},
                "exposure": exposure,
            }),
        );
        self.ok("asset.state", json!({"asset_id": asset}))["current_entry"]["id"].clone()
    }

    fn export(&self, params: Value) -> Value {
        self.ok("export.jpeg", with_envelope(params))
    }

    /// Read an export until it leaves `queued` and `running`. Nothing in the owner polls; this is
    /// the test standing in for a client.
    fn settle(&self, job: &Value) -> Value {
        luxforge_testbase::wait_for("an export to settle", || {
            let read = self.ok("job.read", json!({"job_id": job}));
            (!matches!(read["status"].as_str(), Some("queued" | "running"))).then_some(read)
        })
    }

    /// Hold every export accepted from now on as it begins `phase`: `reached` receives once per
    /// job there, and each send on the returned sender lets one go on.
    fn hold_at(&self, phase: &'static str) -> (mpsc::Receiver<()>, SyncSender<()>) {
        let (reached, reaches) = mpsc::channel();
        let (release, released) = sync_channel::<()>(64);
        let reached = Mutex::new(reached);
        let released = Mutex::new(released);
        self.owner.hold_exports(Some(Arc::new(move |at| {
            if at == phase {
                let _ = reached.lock().unwrap().send(());
                let _ = released.lock().unwrap().recv();
            }
        })));
        (reaches, release)
    }

    /// The events recorded so far, as `(method, request id)`.
    fn events(&self) -> Vec<(String, String)> {
        self.ok("events.since", json!({"after": 0}))["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|event| {
                (
                    event["method"].as_str().unwrap().to_owned(),
                    event["request_id"].as_str().unwrap().to_owned(),
                )
            })
            .collect()
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        if self.join.is_some() {
            self.owner.hold_exports(None);
            self.stop();
        }
        // A reopened harness took the directory with it.
        if !self.dir.as_os_str().is_empty() {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }
}

fn request_envelope() -> Value {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    json!({
        "request_id": format!("export-test-{}", NEXT.fetch_add(1, Ordering::Relaxed)),
        "actor": "test",
    })
}

fn with_envelope(mut params: Value) -> Value {
    if params.get("mutation").is_none() {
        params["mutation"] = request_envelope();
    }
    params
}

/// The renderer a written export's result names on an owner with no GPU provider: the reference
/// renderer, with no reason, since it is the only one there.
fn reference_renderer() -> Value {
    json!({"record": "reference", "reason": null})
}

/// The SHA-256 of a file's bytes, as hex.
fn digest(path: &Path) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(fs::read(path).unwrap()))
}

/// The names in a directory, sorted.
fn listing(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// A fresh directory for export destinations only, so a leftover temporary file shows.
fn destinations(harness: &Harness) -> PathBuf {
    let dir = harness.dir.join("exports");
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// Whether a JPEG carries an APP1 segment (EXIF or XMP) before its image data.
fn has_app1(bytes: &[u8]) -> bool {
    assert_eq!(&bytes[..2], [0xff, 0xd8], "a JPEG starts with SOI");
    let mut at = 2;
    while at + 4 <= bytes.len() {
        assert_eq!(bytes[at], 0xff, "a marker at {at}");
        let marker = bytes[at + 1];
        if marker == 0xda {
            return false;
        }
        if marker == 0xe1 {
            return true;
        }
        let length = usize::from(u16::from_be_bytes([bytes[at + 2], bytes[at + 3]]));
        at += 2 + length;
    }
    false
}

/// The exact render of one saved entry, from a service of its own over the stopped owner's
/// catalog, prepared by the blocking helper: the independent reference the exported file is
/// compared against.
fn reference(catalog: &Path, asset: &Value, entry: &Value) -> crate::Raster {
    let mut service = EditorService::open(catalog).unwrap();
    let asset: AssetId = serde_json::from_value(asset.clone()).unwrap();
    let entry: EntryId = serde_json::from_value(entry.clone()).unwrap();
    service
        .prepare(&service.entry_needs(&asset, Some(&entry)).unwrap())
        .unwrap();
    service.render_entry(&asset, &entry).unwrap()
}

/// Per-channel mean and maximum absolute difference between a decoded export and a frame, and the
/// share of pixels any channel of which differs by more than [`NEAR`].
fn difference(path: &Path, frame: &crate::Raster) -> ([f64; 3], [u8; 3], f64) {
    let decoded = image::open(path).unwrap().to_rgb8();
    assert_eq!(
        (decoded.width(), decoded.height()),
        (frame.width, frame.height),
        "the export is the output stage's size"
    );
    let mut sum = [0u64; 3];
    let mut max = [0u8; 3];
    let mut far = 0u32;
    for (exported, rendered) in decoded.pixels().zip(frame.rgba.chunks_exact(4)) {
        let mut near = true;
        for channel in 0..3 {
            let delta = exported.0[channel].abs_diff(rendered[channel]);
            sum[channel] += u64::from(delta);
            max[channel] = max[channel].max(delta);
            near &= delta <= NEAR;
        }
        far += u32::from(!near);
    }
    let count = f64::from(frame.width) * f64::from(frame.height);
    (
        sum.map(|total| total as f64 / count),
        max,
        f64::from(far) / count,
    )
}

/// What quality 90 at full-resolution chroma may change: measured on the quadrant fixture, whose
/// only detail is its hard quadrant edges. Away from them an export decodes within a code or two
/// of the render (per-channel means 0.4 to 1.2); the blocks straddling an edge ring, which is
/// where about 3.6% of the pixels differ by more than [`NEAR`] and the maximum of 65 is.
fn assert_matches(path: &Path, frame: &crate::Raster, what: &str) {
    assert_encodes(path, frame, what);
    let (mean, max, far) = difference(path, frame);
    eprintln!("{what}: mean {mean:?}, max {max:?}, share beyond {NEAR}: {far}");
    for channel in 0..3 {
        assert!(mean[channel] < MEAN_TOLERANCE, "{what}: mean {mean:?}");
        assert!(max[channel] <= MAX_TOLERANCE, "{what}: max {max:?}");
    }
    assert!(far < FAR_SHARE, "{what}: {far} of the pixels beyond {NEAR}");
}

/// A file written without metadata is byte for byte the encoder's output for the reference frame:
/// the job rendered exactly that frame and added nothing to it.
fn assert_encodes(path: &Path, frame: &crate::Raster, what: &str) {
    let mut expected = Vec::new();
    crate::export::encode::encode_jpeg(&mut expected, frame, None, None, &mut |_| {}, &|| Ok(()))
        .unwrap();
    assert!(
        fs::read(path).unwrap() == expected,
        "{what}: the file is not the encoding of the reference frame"
    );
}

const MEAN_TOLERANCE: f64 = 1.5;
const MAX_TOLERANCE: u8 = 80;
const NEAR: u8 = 8;
const FAR_SHARE: f64 = 0.05;

/// Count the real Detail operation on the export lane, then compare its file with the same
/// deterministic encoder reading an independently evaluated exact raster. JPEG decoding is not
/// used as an exact-pixel oracle.
#[test]
fn detail_export_evaluates_exact_detail_once() {
    use sha2::{Digest, Sha256};

    let mut harness = Harness::start("detail-once");
    let state = harness.import("detail-original.jpg");
    let original = harness.dir.join("detail-original.jpg");
    let original_hash = format!("{:x}", Sha256::digest(fs::read(&original).unwrap()));
    let asset = state["asset"]["id"].clone();
    harness.ok(
        "edit.set-detail",
        json!({
            "asset_id":asset,
            "luminance":40,"colour":40,"sharpening":50,"radius":1,
            "mutation":{"expected_revision":0,"request_id":"detail-export-settings","actor":"test"}
        }),
    );
    let edited = harness.ok("asset.state", json!({"asset_id":asset}));
    let entry = edited["current_entry"]["id"].clone();
    let recipe: crate::Recipe =
        serde_json::from_value(edited["current_entry"]["snapshot"]["recipe"].clone()).unwrap();
    assert_eq!(recipe.layers.len(), 1);
    assert_eq!(recipe.layers[0].effect_id, crate::DETAIL_EFFECT);

    // The 480x320 stage fits one spatial tile, so run_tile is called on the export thread even
    // when its internal rows use the pool. This observer counts that worker alone.
    let tiles = Arc::new(AtomicU64::new(0));
    let observed = tiles.clone();
    let (entered, reached) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let entered = Mutex::new(entered);
    let released = Mutex::new(released);
    harness.owner.hold_exports(Some(Arc::new(move |phase| {
        if phase == "rendering" {
            crate::render::spatial::observe_tiles(observed.clone());
            let _ = entered.lock().unwrap().send(());
            let _ = released.lock().unwrap().recv();
        }
    })));
    let destination = destinations(&harness).join("detail.jpg");
    let parameters = json!({
        "asset_id":asset,"entry_id":entry,"destination":destination,
        "mutation":{"request_id":"detail-export-once","actor":"test"}
    });
    let accepted = harness.ok("export.jpeg", parameters.clone());
    reached.recv_timeout(luxforge_testbase::HANG).unwrap();
    assert_eq!(
        tiles.load(Ordering::Relaxed),
        0,
        "the export is held before rendering"
    );
    // The accepted entry stays frozen while a different Detail recipe becomes current.
    harness.ok(
        "edit.set-detail",
        json!({
            "asset_id":asset,"sharpening":120,"luminance":90,
            "mutation":{"expected_revision":1,"request_id":"detail-after-export","actor":"test"}
        }),
    );
    release.send(()).unwrap();
    assert_eq!(harness.settle(&accepted["job_id"])["status"], "ready");
    assert_eq!(
        tiles.load(Ordering::Relaxed),
        1,
        "one exact Detail tile chain"
    );
    let retry = harness.ok("export.jpeg", parameters);
    assert_eq!(retry["job_id"], accepted["job_id"]);
    assert_eq!(retry["deduplicated"], true);
    assert_eq!(
        tiles.load(Ordering::Relaxed),
        1,
        "retry evaluates no Detail tile"
    );

    harness.stop();
    let exact = reference(&harness.catalog, &asset, &entry);
    let source = crate::open_source(&original).unwrap();
    let independently_evaluated = crate::render(
        &ModuleRegistry::builtin(),
        &source,
        &recipe,
        crate::RenderOptions::default(),
        &crate::RenderContext::new(),
    )
    .unwrap()
    .frame(exact.snapshot_id.clone())
    .unwrap();
    assert_eq!(
        exact.rgba, independently_evaluated.rgba,
        "exact saved-entry RGBA"
    );
    assert_ne!(
        exact.rgba, source.rgba,
        "the fixture exercises active Detail"
    );
    assert_encodes(&destination, &exact, "one exact Detail evaluation");
    assert_eq!(
        format!("{:x}", Sha256::digest(fs::read(&original).unwrap())),
        original_hash
    );
}

/// With an export folder remembered, `export.plan` suggests a name there, counting up within it,
/// while it is a folder that exists; otherwise beside the original, as with none remembered.
#[test]
fn export_plan_suggests_the_remembered_folder_only_while_it_exists() {
    let harness = Harness::with_preferences("remembered-folder");
    let state = harness.import("DSC_0042.jpg");
    let asset = state["asset"]["id"].clone();
    let suggested = || harness.ok("export.plan", json!({"asset_id": asset}))["suggested"].clone();
    let beside = json!(harness.dir.join("DSC_0042-edited.jpg"));
    assert_eq!(suggested(), beside, "nothing remembered");
    let folder = harness.dir.join("Exports");
    harness.ok("preferences.set", json!({"export_folder": folder}));
    assert_eq!(suggested(), beside, "a missing folder is not suggested");
    fs::create_dir(&folder).unwrap();
    assert_eq!(suggested(), json!(folder.join("DSC_0042-edited.jpg")));
    fs::write(folder.join("DSC_0042-edited.jpg"), b"taken").unwrap();
    assert_eq!(suggested(), json!(folder.join("DSC_0042-edited-2.jpg")));
    // A file where the folder was is not a folder.
    fs::remove_dir_all(&folder).unwrap();
    fs::write(&folder, b"not a folder").unwrap();
    assert_eq!(suggested(), beside);
    fs::remove_file(&folder).unwrap();
    fs::create_dir(&folder).unwrap();
    harness.ok("preferences.set", json!({"export_folder": null}));
    assert_eq!(suggested(), beside, "forgotten");
}

/// An export plans the entry's output stage and suggests a name beside the original, writes the
/// exact render of that entry as a new JPEG, reports every phase on the board and records one
/// event under the request that asked for it.
#[test]
fn an_export_writes_the_entrys_exact_render_as_a_new_jpeg() {
    let mut harness = Harness::start("exact");
    let state = harness.import("DSC_0042.jpg");
    let asset = state["asset"]["id"].clone();
    let entry = harness.expose(&asset, 0.7);
    let planned = harness.ok("export.plan", json!({"asset_id": asset}));
    let suggested = harness.dir.join("DSC_0042-edited.jpg");
    assert_eq!(
        planned,
        json!({
            "asset_id": asset,
            "entry_id": entry,
            "snapshot_id": planned["snapshot_id"],
            "width": 480,
            "height": 320,
            "suggested": suggested,
        })
    );
    // A taken name is counted past, and the plan never wrote anything.
    fs::write(&suggested, b"taken").unwrap();
    let planned = harness.ok("export.plan", json!({"asset_id": asset}));
    assert_eq!(
        planned["suggested"],
        json!(harness.dir.join("DSC_0042-edited-2.jpg"))
    );
    let destination = PathBuf::from(planned["suggested"].as_str().unwrap());
    let accepted = harness
        .send_as(
            "export-request",
            "export.jpeg",
            json!({"asset_id": asset, "destination": destination, "mutation": request_envelope()}),
        )
        .result
        .expect("the export is accepted");
    assert_eq!(accepted["asset_id"], asset);
    assert_eq!(accepted["entry_id"], entry);
    assert_eq!(accepted["snapshot_id"], planned["snapshot_id"]);
    assert_eq!(accepted["destination"], json!(destination));
    assert_eq!(
        (accepted["width"].clone(), accepted["height"].clone()),
        (json!(480), json!(320))
    );
    assert_eq!(accepted["keep_metadata"], json!(false));
    assert_eq!(accepted["reference"], json!(false));
    assert_eq!(accepted["deduplicated"], json!(false));
    let read = harness.settle(&accepted["job_id"]);
    assert_eq!(read["status"], "ready", "{read}");
    assert_eq!(read["job_id"], accepted["job_id"]);
    let bytes = fs::metadata(&destination).unwrap().len();
    assert_eq!(
        read["result"],
        json!({
            "path": destination, "bytes": bytes, "width": 480, "height": 320, "metadata": [],
            "renderer": reference_renderer(),
        })
    );
    assert!(read.get("error").is_none());
    assert_eq!(
        harness
            .events()
            .into_iter()
            .filter(|(method, _)| method == "export.jpeg")
            .collect::<Vec<_>>(),
        [("export.jpeg".to_owned(), "export-request".to_owned())],
        "a finished export is one event under its request"
    );
    let activity = harness.ok("activity.list", json!({}));
    let finished = activity["recent"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["kind"] == "export")
        .expect("the export is on the board");
    assert_eq!(finished["job_id"], accepted["job_id"]);
    assert_eq!(finished["asset_id"], asset);
    assert_eq!(finished["label"], "Exporting JPEG");
    assert_eq!(finished["detail"], "DSC_0042-edited-2.jpg");
    assert_eq!(finished["phase"], "writing", "it ended in its last phase");
    assert_eq!(finished["outcome"], "completed");
    // Nothing else appeared in the directory: no temporary file and no change to the original or
    // the taken name.
    let photos: Vec<String> = listing(&harness.dir)
        .into_iter()
        .filter(|name| name.starts_with("DSC_") || name.starts_with('.'))
        .collect();
    assert_eq!(
        photos,
        [
            "DSC_0042-edited-2.jpg",
            "DSC_0042-edited.jpg",
            "DSC_0042.jpg"
        ]
    );
    assert_eq!(fs::read(&suggested).unwrap(), b"taken");
    assert_eq!(
        fs::read(harness.dir.join("DSC_0042.jpg")).unwrap(),
        fs::read(fixture()).unwrap()
    );
    harness.stop();
    let frame = reference(&harness.catalog, &asset, &entry);
    assert_matches(&destination, &frame, "the exported entry");
}

/// The entry is frozen when the export is accepted: an edit committed right after, while the
/// export waits behind another, changes neither file, and a historical entry exports as itself
/// while the current entry differs.
#[test]
fn a_later_commit_never_changes_an_accepted_export() {
    let mut harness = Harness::start("frozen");
    let state = harness.import("frozen.jpg");
    let asset = state["asset"]["id"].clone();
    let first = harness.expose(&asset, 0.6);
    let out = destinations(&harness);
    let (reached, release) = harness.hold_at(RENDER_PHASE);
    let running =
        harness.export(json!({"asset_id": asset, "destination": out.join("running.jpg")}));
    reached.recv_timeout(luxforge_testbase::HANG).unwrap();
    let queued = harness.export(json!({"asset_id": asset, "destination": out.join("queued.jpg")}));
    assert_eq!(running["status"], "running");
    assert_eq!(queued["status"], "queued");
    assert_eq!(queued["entry_id"], first);
    // Another edit becomes current while both wait.
    let second = harness.expose(&asset, -0.8);
    assert_ne!(second, first);
    // A historical export names the first entry while the second is current.
    let historical = harness.export(json!({
        "asset_id": asset, "entry_id": first, "destination": out.join("historical.jpg"),
    }));
    assert_eq!(historical["entry_id"], first);
    let current =
        harness.export(json!({"asset_id": asset, "destination": out.join("current.jpg")}));
    assert_eq!(current["entry_id"], second);
    harness.owner.hold_exports(None);
    for _ in 0..4 {
        let _ = release.send(());
    }
    for job in [&running, &queued, &historical, &current] {
        assert_eq!(harness.settle(&job["job_id"])["status"], "ready");
    }
    harness.stop();
    let first_frame = reference(&harness.catalog, &asset, &first);
    let second_frame = reference(&harness.catalog, &asset, &second);
    assert_ne!(
        first_frame.rgba, second_frame.rgba,
        "the two entries differ"
    );
    for name in ["running.jpg", "queued.jpg", "historical.jpg"] {
        assert_matches(&out.join(name), &first_frame, name);
        // And it is not the later entry: the tolerance is far tighter than the two differ.
        let (mean, _, _) = difference(&out.join(name), &second_frame);
        assert!(mean.iter().all(|mean| *mean > 10.0), "{name}: {mean:?}");
    }
    assert_matches(&out.join("current.jpg"), &second_frame, "current.jpg");
}

const RENDER_PHASE: &str = "rendering";

/// A retried `export.jpeg` is answered from the first answer: one job, one file, one event.
#[test]
fn a_retried_export_is_answered_from_the_first_and_writes_one_file() {
    let harness = Harness::start("retry");
    let state = harness.import("retry.jpg");
    let asset = state["asset"]["id"].clone();
    let out = destinations(&harness);
    let params = json!({
        "asset_id": asset, "destination": out.join("once.jpg"),
        "mutation": {"request_id": "export-once", "actor": "test"},
    });
    let first = harness.ok("export.jpeg", params.clone());
    assert_eq!(harness.settle(&first["job_id"])["status"], "ready");
    let events = harness.events();
    // The file exists now, so running the handler again would be a conflict: the retry is
    // answered from the table instead.
    let retried = harness.ok("export.jpeg", params.clone());
    assert_eq!(retried["deduplicated"], json!(true));
    let mut original = retried.clone();
    original["deduplicated"] = json!(false);
    assert_eq!(original, first, "the retry is the first answer");
    assert_eq!(harness.events(), events, "the retry records no event");
    assert_eq!(listing(&out), ["once.jpg"]);
    let mut other = params;
    other["destination"] = json!(out.join("other.jpg"));
    let conflict = harness.refused("export.jpeg", other);
    assert_eq!(conflict.code, "conflict");
    assert_eq!(
        conflict.message,
        "request_id was already used with different input"
    );
    assert_eq!(listing(&out), ["once.jpg"]);
}

/// `schema.list` lists `reference` as an optional boolean, default false, whose notes say what it
/// asks for, and a value of another kind is refused. The answer echoes it, and a written export's
/// result names the renderer that wrote the file: on an owner with no GPU provider the reference
/// renderer either way, for no reason without the option and as `requested` with it, so asking for
/// it writes the same file as not asking. Request deduplication hashes it as it does every
/// parameter: the same request again is answered from the first answer, and the same request id
/// with another value is a conflict. The original is unchanged throughout.
#[test]
fn the_reference_option_is_listed_echoed_and_hashed() {
    let harness = Harness::start("reference");
    let state = harness.import("reference.jpg");
    let asset = state["asset"]["id"].clone();
    let original = harness.dir.join("reference.jpg");
    let original_digest = digest(&original);
    harness.expose(&asset, 0.5);
    let out = destinations(&harness);

    let schema = harness.ok("schema.list", json!({}));
    let listed = &schema["methods"]["export.jpeg"];
    let parameter = listed["parameters"]
        .as_array()
        .unwrap()
        .iter()
        .find(|parameter| parameter["name"] == "reference")
        .expect("export.jpeg lists reference");
    assert_eq!(
        (
            parameter["kind"].clone(),
            parameter["required"].clone(),
            parameter["default"].clone()
        ),
        (json!("boolean"), json!(false), json!(false))
    );
    let notes = parameter["notes"].as_str().unwrap();
    assert!(
        notes.contains("reference renderer") && notes.contains("requested"),
        "{notes}"
    );
    assert_eq!(listed["optional"]["reference"], json!(notes));
    let refused = harness.refused(
        "export.jpeg",
        with_envelope(json!({
            "asset_id": asset, "destination": out.join("refused.jpg"), "reference": "yes",
        })),
    );
    assert_eq!(
        (refused.code.as_str(), refused.message.as_str()),
        ("validation", "parameter reference must be a boolean")
    );

    let default =
        harness.export(json!({"asset_id": asset, "destination": out.join("default.jpg")}));
    let asked = harness.export(json!({
        "asset_id": asset, "destination": out.join("asked.jpg"), "reference": true,
    }));
    assert_eq!(default["reference"], json!(false));
    assert_eq!(asked["reference"], json!(true));
    for (job, renderer) in [
        (&default, reference_renderer()),
        (
            &asked,
            json!({"record": "reference", "reason": "requested"}),
        ),
    ] {
        let read = harness.settle(&job["job_id"]);
        assert_eq!(read["status"], "ready", "{read}");
        assert_eq!(read["result"]["renderer"], renderer);
    }
    assert!(
        fs::read(out.join("asked.jpg")).unwrap() == fs::read(out.join("default.jpg")).unwrap(),
        "the same file either way"
    );

    let params = json!({
        "asset_id": asset, "destination": out.join("hashed.jpg"), "reference": true,
        "mutation": {"request_id": "export-reference", "actor": "test"},
    });
    let first = harness.ok("export.jpeg", params.clone());
    assert_eq!(harness.settle(&first["job_id"])["status"], "ready");
    let retried = harness.ok("export.jpeg", params.clone());
    assert_eq!(retried["deduplicated"], json!(true));
    assert_eq!(retried["job_id"], first["job_id"]);
    assert_eq!(retried["reference"], json!(true));
    let mut declined = params;
    declined["reference"] = json!(false);
    let conflict = harness.refused("export.jpeg", declined);
    assert_eq!(
        (conflict.code.as_str(), conflict.message.as_str()),
        (
            "conflict",
            "request_id was already used with different input"
        )
    );
    assert_eq!(listing(&out), ["asked.jpg", "default.jpg", "hashed.jpg"]);
    assert_eq!(
        digest(&original),
        original_digest,
        "the original is unchanged"
    );
}

/// The quadrant fixture with its EXIF segment replaced by one a camera might write: supported
/// capture fields beside a maker note and a serial number, which an export never carries.
fn camera_jpeg() -> Vec<u8> {
    use exif::{Field, In, Tag, Value as ExifValue, experimental::Writer};
    let ascii = |text: &str| ExifValue::Ascii(vec![text.as_bytes().to_vec()]);
    let fields = [
        (Tag::Orientation, ExifValue::Short(vec![1])),
        (Tag::Make, ascii("NIKON CORPORATION")),
        (Tag::Model, ascii("NIKON Z 6")),
        (Tag::Artist, ascii("Will")),
        (Tag::MakerNote, ExifValue::Undefined(vec![7; 16], 0)),
        (Tag::BodySerialNumber, ascii("6000123")),
    ]
    .map(|(tag, value)| Field {
        tag,
        ifd_num: In::PRIMARY,
        value,
    });
    let mut writer = Writer::new();
    for field in &fields {
        writer.push_field(field);
    }
    let mut tiff = std::io::Cursor::new(Vec::new());
    writer.write(&mut tiff, false).unwrap();
    let payload = [b"Exif\0\0".as_slice(), &tiff.into_inner()].concat();
    let original = fs::read(fixture()).unwrap();
    // SOI and the JFIF APP0, then the fixture's own EXIF APP1, which is replaced.
    let app0 = 2 + 2 + usize::from(u16::from_be_bytes([original[4], original[5]]));
    assert_eq!(
        original[app0 + 1],
        0xe1,
        "the fixture's EXIF follows its JFIF header"
    );
    let app1 = app0 + 2 + usize::from(u16::from_be_bytes([original[app0 + 2], original[app0 + 3]]));
    let mut out = original[..app0].to_vec();
    out.extend_from_slice(&[0xff, 0xe1]);
    out.extend_from_slice(&u16::try_from(payload.len() + 2).unwrap().to_be_bytes());
    out.extend_from_slice(&payload);
    out.extend_from_slice(&original[app1..]);
    out
}

/// Metadata is off by default: no APP1 segment at all. With Keep metadata the file carries one
/// EXIF segment, read back by an independent reader: the original's supported fields, the
/// structural ones with Orientation 1 and the output's size, and no maker note or serial number.
#[test]
fn keep_metadata_writes_one_exif_segment_and_the_default_writes_none() {
    let harness = Harness::start("metadata");
    let original = harness.dir.join("camera.jpg");
    fs::write(&original, camera_jpeg()).unwrap();
    let asset = harness.opened(&original)["asset"]["id"].clone();
    let out = destinations(&harness);
    let stripped =
        harness.export(json!({"asset_id": asset, "destination": out.join("stripped.jpg")}));
    let kept = harness.export(json!({
        "asset_id": asset, "destination": out.join("kept.jpg"), "keep_metadata": true,
    }));
    assert_eq!(stripped["keep_metadata"], json!(false));
    assert_eq!(kept["keep_metadata"], json!(true));
    let stripped = harness.settle(&stripped["job_id"]);
    let kept = harness.settle(&kept["job_id"]);
    assert_eq!(stripped["result"]["metadata"], json!([]));
    assert!(!has_app1(&fs::read(out.join("stripped.jpg")).unwrap()));
    let bytes = fs::read(out.join("kept.jpg")).unwrap();
    let written: Vec<&str> = kept["result"]["metadata"]
        .as_array()
        .unwrap()
        .iter()
        .map(|name| name.as_str().unwrap())
        .collect();
    assert_eq!(written, ["Make", "Model", "Artist"]);
    let exif = exif::Reader::new()
        .read_from_container(&mut std::io::Cursor::new(&bytes))
        .expect("the independent reader finds the EXIF segment");
    let value = |tag: exif::Tag| {
        exif.get_field(tag, exif::In::PRIMARY)
            .map(|field| field.display_value().to_string())
    };
    assert_eq!(
        value(exif::Tag::Make).as_deref(),
        Some("\"NIKON CORPORATION\"")
    );
    assert_eq!(value(exif::Tag::Model).as_deref(), Some("\"NIKON Z 6\""));
    assert_eq!(value(exif::Tag::Artist).as_deref(), Some("\"Will\""));
    assert_eq!(
        value(exif::Tag::Orientation).as_deref(),
        Some("row 0 at top and column 0 at left")
    );
    assert_eq!(value(exif::Tag::PixelXDimension).as_deref(), Some("480"));
    assert_eq!(value(exif::Tag::PixelYDimension).as_deref(), Some("320"));
    assert_eq!(value(exif::Tag::MakerNote), None);
    assert_eq!(value(exif::Tag::BodySerialNumber), None);
}

/// The JFIF header's density, `(units, x, y)`.
fn jfif_density(bytes: &[u8]) -> (u8, u16, u16) {
    let app0 = luxforge_jpeg::segments(bytes)
        .map(Result::unwrap)
        .find(|segment| segment.marker == 0xe0)
        .expect("a JFIF header");
    let density = |at: usize| u16::from_be_bytes([app0.payload[at], app0.payload[at + 1]]);
    (app0.payload[7], density(8), density(10))
}

/// By default the JFIF header names no density, a unitless 1:1 aspect ratio; asked for one, it
/// carries that many pixels per inch both ways and the file is otherwise the same bytes. A density
/// outside the JFIF field is a validation error and writes nothing.
#[test]
fn pixels_per_inch_sets_the_jfif_density_and_nothing_else() {
    let harness = Harness::start("density");
    let state = harness.import("density.jpg");
    let asset = state["asset"]["id"].clone();
    let out = destinations(&harness);
    let plain = harness.export(json!({"asset_id": asset, "destination": out.join("plain.jpg")}));
    let dense = harness.export(json!({
        "asset_id": asset, "destination": out.join("dense.jpg"), "pixels_per_inch": 144,
    }));
    assert_eq!(plain["pixels_per_inch"], Value::Null);
    assert_eq!(dense["pixels_per_inch"], json!(144));
    harness.settle(&plain["job_id"]);
    harness.settle(&dense["job_id"]);
    let plain = fs::read(out.join("plain.jpg")).unwrap();
    let dense = fs::read(out.join("dense.jpg")).unwrap();
    assert_eq!(jfif_density(&plain), (0, 1, 1));
    assert_eq!(jfif_density(&dense), (1, 144, 144));
    let differing: Vec<usize> = (0..plain.len().max(dense.len()))
        .filter(|&at| plain.get(at) != dense.get(at))
        .collect();
    assert_eq!(
        differing,
        [13, 15, 17],
        "only the units and the densities' low bytes"
    );
    for density in [0, 65536] {
        let refused = harness.refused(
            "export.jpeg",
            with_envelope(json!({
                "asset_id": asset, "destination": out.join("refused.jpg"),
                "pixels_per_inch": density,
            })),
        );
        assert_eq!(refused.code, "validation", "{density}: {refused:?}");
    }
    assert!(!out.join("refused.jpg").exists());
}

/// Every obvious refusal is answered at once and writes nothing: the destination's shape, an
/// existing file, an unknown asset or entry, a missing original, and a stack whose provider is
/// unavailable. An unprepared source is `preparation-required` with the job that prepares it,
/// after which the same request is accepted.
#[test]
fn refusals_are_answered_at_once_and_write_nothing() {
    let harness = Harness::start("refusals");
    let state = harness.import("refused.jpg");
    let asset = state["asset"]["id"].clone();
    harness.expose(&asset, 0.3);
    let out = destinations(&harness);
    let existing = out.join("existing.jpg");
    fs::write(&existing, b"keep me").unwrap();
    let export = |destination: Value| {
        harness.refused(
            "export.jpeg",
            with_envelope(json!({"asset_id": asset, "destination": destination})),
        )
    };
    let conflict = export(json!(existing));
    assert_eq!(conflict.code, "conflict", "{conflict:?}");
    assert_eq!(fs::read(&existing).unwrap(), b"keep me");
    let original = export(json!(harness.dir.join("refused.jpg")));
    assert_eq!(
        original.code, "conflict",
        "the original itself: {original:?}"
    );
    // The shape is a validation error; a parent that cannot be read, a missing one included, is
    // the file-system error that says so.
    for (destination, what, code) in [
        (json!("relative.jpg"), "a relative path", "validation"),
        (
            json!(out.join("wrong.png")),
            "a wrong extension",
            "validation",
        ),
        (
            json!(out.join("missing").join("x.jpg")),
            "a missing parent",
            "read-error",
        ),
    ] {
        let refused = export(destination);
        assert_eq!(refused.code, code, "{what}: {refused:?}");
    }
    let unknown = harness.refused(
        "export.jpeg",
        with_envelope(json!({"asset_id": AssetId::new(), "destination": out.join("a.jpg")})),
    );
    assert_eq!(unknown.code, "validation", "{unknown:?}");
    let unknown_entry = harness.refused(
        "export.jpeg",
        with_envelope(json!({
            "asset_id": asset, "entry_id": EntryId::new(), "destination": out.join("b.jpg"),
        })),
    );
    assert_eq!(unknown_entry.code, "validation", "{unknown_entry:?}");
    let unknown_plan = harness.refused(
        "export.plan",
        json!({"asset_id": asset, "entry_id": EntryId::new()}),
    );
    assert_eq!(unknown_plan.code, unknown_entry.code);
    assert_eq!(
        harness
            .refused("job.read", json!({"job_id": JobId::new()}))
            .code,
        "validation"
    );
    assert_eq!(
        harness
            .refused("job.cancel", json!({"job_id": JobId::new()}))
            .code,
        "validation"
    );
    // A missing original: the cached source is never exported without its original.
    let moved = harness.dir.join("moved.jpg");
    fs::rename(harness.dir.join("refused.jpg"), &moved).unwrap();
    let missing = export(json!(out.join("c.jpg")));
    assert_eq!(missing.code, "source-unavailable", "{missing:?}");
    fs::rename(&moved, harness.dir.join("refused.jpg")).unwrap();
    assert_eq!(listing(&out), ["existing.jpg"]);

    // Reopened, the source is not prepared: the export is deferred to the job that prepares it,
    // and the same request is accepted once that job is ready.
    let harness = harness.reopen(Arc::new(ModuleRegistry::builtin()));
    let params = with_envelope(json!({"asset_id": asset, "destination": out.join("later.jpg")}));
    let deferred = harness.refused("export.jpeg", params.clone());
    assert_eq!(deferred.code, "preparation-required", "{deferred:?}");
    let job = deferred.job_id.expect("the preparation job");
    assert_eq!(harness.settle_source(&json!(job))["status"], "ready");
    let accepted = harness.ok("export.jpeg", params);
    assert_eq!(harness.settle(&accepted["job_id"])["status"], "ready");
    assert_eq!(listing(&out), ["existing.jpg", "later.jpg"]);
}

/// A stack whose provider is unavailable cannot be evaluated: the plan and the export both refuse
/// with the reason, nothing is queued and nothing is written, and the layer is kept.
#[test]
fn an_unavailable_provider_refuses_the_export_and_writes_nothing() {
    let harness = Harness::start("unavailable");
    let state = harness.import("unavailable.jpg");
    let asset = state["asset"]["id"].clone();
    harness.ok(
        "edit.set-pixel",
        json!({
            "asset_id": asset,
            "mutation": {"expected_revision": 0, "request_id": "pixel", "actor": "test"},
            "x": 0, "y": 0, "rgb": [1, 2, 3],
        }),
    );
    let out = destinations(&harness);
    let mut registry = ModuleRegistry::new();
    registry
        .register(crate::modules::TestModule::shared(
            "luxforge.pixel",
            crate::PIXEL_EFFECT,
            "set-pixel",
            crate::Availability::Unavailable {
                reason: "test: the pixel provider is not installed".into(),
            },
        ))
        .unwrap();
    let harness = harness.reopen(Arc::new(registry));
    let planned = harness.refused("export.plan", json!({"asset_id": asset}));
    assert_eq!(planned.code, "incompatible", "{planned:?}");
    let refused = harness.refused(
        "export.jpeg",
        with_envelope(json!({"asset_id": asset, "destination": out.join("x.jpg")})),
    );
    assert_eq!(refused.code, "incompatible", "{refused:?}");
    assert!(
        refused.message.contains("unavailable effect"),
        "{}",
        refused.message
    );
    assert!(
        refused.job_id.is_none(),
        "nothing was queued, not even a preparation"
    );
    assert!(listing(&out).is_empty());
    assert!(
        harness.ok("activity.list", json!({}))["recent"]
            .as_array()
            .unwrap()
            .iter()
            .all(|entry| entry["kind"] != "export")
    );
}

/// A queued export cancelled never starts; a running one stops at its next block and removes its
/// temporary file; a finished one is answered unchanged. Any client may cancel.
#[test]
fn a_cancelled_export_leaves_no_file_and_no_temporary_file() {
    let harness = Harness::start("cancel");
    let state = harness.import("cancel.jpg");
    let asset = state["asset"]["id"].clone();
    let out = destinations(&harness);
    // Held after its temporary file was staged, as the encoding begins.
    let (reached, release) = harness.hold_at("encoding");
    let running =
        harness.export(json!({"asset_id": asset, "destination": out.join("running.jpg")}));
    reached.recv_timeout(luxforge_testbase::HANG).unwrap();
    let queued = harness.export(json!({"asset_id": asset, "destination": out.join("queued.jpg")}));
    assert_eq!(queued["status"], "queued");
    let staged = listing(&out);
    assert_eq!(staged.len(), 1, "one temporary file: {staged:?}");
    assert!(staged[0].starts_with(".running.jpg."), "{staged:?}");

    let other = harness.owner.register();
    let as_other = |method: &str, job: &Value| {
        harness
            .owner
            .call(
                other,
                ApiRequest {
                    id: method.into(),
                    method: method.into(),
                    params: json!({"job_id": job}),
                    token: None,
                },
            )
            .unwrap()
            .result
            .unwrap_or_else(|| panic!("any client may {method}"))
    };
    let removed = as_other("job.cancel", &queued["job_id"]);
    assert_eq!(removed["status"], "cancelled");
    assert_eq!(removed["error"]["code"], "cancelled");
    assert_eq!(removed["error"]["message"], "the export was cancelled");
    let requested = as_other("job.cancel", &running["job_id"]);
    assert_eq!(requested["status"], "running", "it stops at its next block");
    release.send(()).unwrap();
    let stopped = harness.settle(&running["job_id"]);
    assert_eq!(stopped["status"], "cancelled", "{stopped}");
    assert_eq!(stopped["error"]["message"], "the export was cancelled");
    assert!(stopped.get("result").is_none());
    assert!(listing(&out).is_empty(), "{:?}", listing(&out));
    assert_eq!(
        as_other("job.read", &queued["job_id"])["status"],
        "cancelled",
        "a cancelled queued job never ran"
    );
    // A finished job is answered as it is.
    harness.owner.hold_exports(None);
    let done = harness.export(json!({"asset_id": asset, "destination": out.join("done.jpg")}));
    assert_eq!(harness.settle(&done["job_id"])["status"], "ready");
    assert_eq!(as_other("job.cancel", &done["job_id"])["status"], "ready");
    assert_eq!(listing(&out), ["done.jpg"]);
    let activity = harness.ok("activity.list", json!({}));
    let outcome = |job: &Value| {
        activity["recent"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["job_id"] == *job)
            .map(|entry| entry["outcome"].clone())
    };
    assert_eq!(outcome(&running["job_id"]), Some(json!("cancelled")));
    assert_eq!(outcome(&queued["job_id"]), None, "a queued job never began");
}

/// The export lane runs one job and holds four; a sixth is refused with `resource-limit` and
/// writes nothing. A requesting client that disconnects leaves its exports running.
#[test]
fn the_export_lane_holds_four_refuses_the_sixth_and_outlives_its_client() {
    let harness = Harness::start("bound");
    let state = harness.import("bound.jpg");
    let asset = state["asset"]["id"].clone();
    let out = destinations(&harness);
    let (reached, release) = harness.hold_at(RENDER_PHASE);
    let mut accepted = Vec::new();
    for index in 0..5 {
        let job = harness.export(json!({
            "asset_id": asset, "destination": out.join(format!("{index}.jpg")),
        }));
        if index == 0 {
            reached.recv_timeout(luxforge_testbase::HANG).unwrap();
            assert_eq!(job["status"], "running");
        } else {
            assert_eq!(job["status"], "queued");
        }
        accepted.push(job);
    }
    let refused = harness.refused(
        "export.jpeg",
        with_envelope(json!({"asset_id": asset, "destination": out.join("5.jpg")})),
    );
    assert_eq!(refused.code, "resource-limit", "{refused:?}");
    assert_eq!(refused.message, "the export lane is full");
    // The requesting client leaves; its exports do not.
    harness.owner.disconnect(harness.client);
    harness.owner.hold_exports(None);
    for _ in 0..5 {
        let _ = release.send(());
    }
    let reader = harness.owner.register();
    for job in &accepted {
        luxforge_testbase::wait_until("an export to settle", || {
            let read = harness
                .owner
                .call(
                    reader,
                    ApiRequest {
                        id: "read".into(),
                        method: "job.read".into(),
                        params: json!({"job_id": job["job_id"]}),
                        token: None,
                    },
                )
                .unwrap()
                .result
                .expect("any client may read an export");
            match read["status"].as_str() {
                Some("ready") => true,
                Some("queued" | "running") => false,
                _ => panic!("an export of a gone client failed: {read}"),
            }
        });
    }
    assert_eq!(listing(&out), ["0.jpg", "1.jpg", "2.jpg", "3.jpg", "4.jpg"]);
}

/// Stopping the owner cancels a running export at its next block and joins the lane: the
/// temporary file is removed and no final file is written.
#[test]
fn stopping_the_owner_cancels_a_running_export_and_leaves_no_file() {
    let mut harness = Harness::start("shutdown");
    let state = harness.import("shutdown.jpg");
    let asset = state["asset"]["id"].clone();
    let out = destinations(&harness);
    let (reached, release) = harness.hold_at("encoding");
    harness.export(json!({"asset_id": asset, "destination": out.join("closing.jpg")}));
    reached.recv_timeout(luxforge_testbase::HANG).unwrap();
    assert_eq!(listing(&out).len(), 1, "staged");
    harness.owner.stop();
    // A call after the stop is dropped unanswered once the owner has asked every live job to stop
    // and closed its channel, so the export is cancelled before it is let go; it then stops at its
    // next check.
    assert!(
        harness
            .owner
            .call(
                harness.client,
                ApiRequest {
                    id: "after-stop".into(),
                    method: "catalog.info".into(),
                    params: json!({}),
                    token: None,
                },
            )
            .is_err(),
        "the stopped owner answers nothing"
    );
    release.send(()).unwrap();
    harness.join.take().unwrap().join().unwrap();
    assert!(listing(&out).is_empty(), "{:?}", listing(&out));
}

/// On a real RAW file: the export renders through the linear domain and matches the exact render
/// of the same entry. Run with LUXFORGE_RAW_FIXTURE pointing to a private qualified NEF, RAF or
/// DNG.
#[test]
#[ignore = "requires a photo-sized RAW fixture; run explicitly on the owner's Mac"]
fn a_raw_export_matches_the_exact_render() {
    let raw = PathBuf::from(std::env::var("LUXFORGE_RAW_FIXTURE").expect("LUXFORGE_RAW_FIXTURE"));
    let original = fs::read(&raw).unwrap();
    let mut harness = Harness::start("raw");
    let opened = harness.opened(&raw);
    let asset = opened["asset"]["id"].clone();
    let original_entry = opened["current_entry"]["id"].clone();
    harness.expose(&asset, 1.0);
    let revision = harness.ok("asset.state", json!({"asset_id": asset}))["revision"].clone();
    harness.ok("edit.set-presence", json!({
        "asset_id": asset, "texture": 30, "clarity": 25, "dehaze": 10,
        "mutation": {"expected_revision": revision, "request_id": "raw-presence", "actor": "test"},
    }));
    let entry =
        harness.ok("asset.state", json!({"asset_id": asset}))["current_entry"]["id"].clone();
    let out = destinations(&harness);
    let params = with_envelope(json!({
        "asset_id": asset, "destination": out.join("raw.jpg"), "keep_metadata": true,
    }));
    let accepted = match harness.send("export.jpeg", params.clone()) {
        ApiResponse {
            error: Some(error), ..
        } if error.code == "preparation-required" => {
            let job = json!(error.job_id.expect("the development job"));
            assert_eq!(harness.settle_source(&job)["status"], "ready");
            harness.ok("export.jpeg", params)
        }
        response => response.result.expect("the export is accepted"),
    };
    let plain = harness.export(json!({"asset_id": asset, "destination": out.join("plain.jpg")}));
    let read = harness.settle(&accepted["job_id"]);
    assert_eq!(read["status"], "ready", "{read}");
    assert_eq!(harness.settle(&plain["job_id"])["status"], "ready");
    eprintln!("RAW export metadata: {}", read["result"]["metadata"]);
    harness.stop();
    let frame = reference(&harness.catalog, &asset, &entry);
    assert_encodes(&out.join("plain.jpg"), &frame, "the RAW export");
    let before = reference(&harness.catalog, &asset, &original_entry);
    assert_ne!(
        frame.rgba, before.rgba,
        "Basic and Presence change the exact RAW pixels"
    );
    assert_eq!(
        fs::read(&raw).unwrap(),
        original,
        "the RAW original is unchanged"
    );
    assert!(!has_app1(&fs::read(out.join("plain.jpg")).unwrap()));
    // The kept file's tags, read by the independent parser: upright, the output's size, the
    // camera's own fields, and nothing the export never carries.
    let kept = fs::read(out.join("raw.jpg")).unwrap();
    let exif = exif::Reader::new()
        .read_from_container(&mut std::io::Cursor::new(&kept))
        .expect("the independent reader finds the EXIF segment");
    let value = |tag: exif::Tag| {
        exif.get_field(tag, exif::In::PRIMARY)
            .map(|field| field.display_value().to_string())
    };
    assert_eq!(
        value(exif::Tag::Orientation).as_deref(),
        Some("row 0 at top and column 0 at left")
    );
    assert_eq!(
        value(exif::Tag::PixelXDimension),
        Some(frame.width.to_string())
    );
    assert_eq!(
        value(exif::Tag::PixelYDimension),
        Some(frame.height.to_string())
    );
    assert!(value(exif::Tag::Make).is_some() && value(exif::Tag::Model).is_some());
    for tag in [
        exif::Tag::MakerNote,
        exif::Tag::BodySerialNumber,
        exif::Tag::JPEGInterchangeFormat,
        exif::Tag::CFAPattern,
    ] {
        assert_eq!(value(tag), None, "{tag} is not carried");
    }
    assert!(
        exif.get_field(exif::Tag::ImageWidth, exif::In::THUMBNAIL)
            .is_none()
    );
    // The decoded error is the encoder's alone, which grows with a photograph's fine texture
    // and noise; it is reported, and the byte equality above is the proof.
    let (mean, max, far) = difference(&out.join("raw.jpg"), &frame);
    eprintln!("RAW export: mean {mean:?}, max {max:?}, share beyond {NEAR}: {far}");
}

/// A metadata-bearing generated JPEG selects the real pinned record through the same query and
/// action as the desktop. It is a numerical fixture, not photographic profile qualification.
fn lens_asset(h: &Harness) -> (Value, PathBuf) {
    let path = h.dir.join("lens-original.jpg");
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/geometry/z6-24-70-35mm-grid.jpg"),
        &path,
    )
    .unwrap();
    let asset = h.opened(&path)["asset"]["id"].clone();
    let profiles =
        luxforge_testbase::wait_for("the offline lens index to finish its one parse", || {
            let response = h.send(
                "query.lens-profiles",
                json!({"asset_id":asset,"assume-uncorrected":true}),
            );
            if let Some(error) = response.error {
                assert_eq!(error.code, "not-ready", "{error:?}");
                None
            } else {
                response.result
            }
        });
    let row = &profiles["status"]["suggestion"];
    assert!(
        row["match"] == "lens-model" && row["eligible"] == true,
        "{profiles}"
    );
    h.ok("edit.select-lens-profile",json!({"asset_id":asset,"profile":row["key"],"assume-uncorrected":true,"mutation":{"expected_revision":0,"request_id":"lens-selection","actor":"test"}}));
    let revision = h.ok("asset.state", json!({"asset_id":asset}))["revision"].clone();
    h.ok("edit.set-perspective",json!({"asset_id":asset,"horizontal":40,"vertical":-25,"mutation":{"expected_revision":revision,"request_id":"perspective-selection","actor":"test"}}));
    (asset, path)
}
#[test]
fn warped_export_cancellation_publishes_nothing() {
    let h = Harness::start("lens-cancel");
    let (asset, original) = lens_asset(&h);
    let original_bytes = fs::read(&original).unwrap();
    let entry = h.ok("asset.state", json!({"asset_id":asset}))["current_entry"].clone();
    for phase in ["rendering", "encoding", "writing"] {
        let (reaches, release) = h.hold_at(phase);
        let destination = h.dir.join(format!("lens-cancel-{phase}.jpg"));
        let job = h.export(json!({"asset_id":asset,"destination":destination}));
        reaches
            .recv_timeout(Duration::from_secs(30))
            .expect("the named export phase");
        h.ok("job.cancel", json!({"job_id":job["job_id"]}));
        release.send(()).unwrap();
        assert_eq!(h.settle(&job["job_id"])["status"], "cancelled");
        assert!(!destination.exists());
        assert_eq!(
            h.ok("asset.state", json!({"asset_id":asset}))["current_entry"],
            entry
        );
        assert_eq!(fs::read(&original).unwrap(), original_bytes);
    }
    let leftovers: Vec<_> = fs::read_dir(&h.dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
        .filter(|name| name.contains(".luxforge-export"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}
#[test]
fn export_refuses_forged_lens_payload_before_publication() {
    let mut h = Harness::start("lens-forged");
    let (asset, original) = lens_asset(&h);
    let original_bytes = fs::read(&original).unwrap();
    let current = h.ok("asset.state", json!({"asset_id":asset}))["current_entry"].clone();
    let mut forged = current.clone();
    let lens = forged["snapshot"]["recipe"]["layers"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|l| l["effect_id"] == crate::LENS_EFFECT)
        .unwrap();
    lens["payload"]["profile"]["normalization"]["unit_scale"] = json!(4.0);
    h.stop();
    let connection = rusqlite::Connection::open(&h.catalog).unwrap();
    // Fault injection into an isolated catalog: production history remains immutable.
    connection
        .execute_batch("DROP TRIGGER entries_are_immutable;")
        .unwrap();
    connection
        .execute(
            "UPDATE entries SET entry_json=?1 WHERE id=?2",
            rusqlite::params![forged.to_string(), current["id"].as_str().unwrap()],
        )
        .unwrap();
    connection.execute_batch("CREATE TRIGGER entries_are_immutable BEFORE UPDATE ON entries BEGIN SELECT RAISE(ABORT, 'history entries are immutable'); END;").unwrap();
    drop(connection);
    h = h.reopen(Arc::new(ModuleRegistry::builtin()));
    let destination = h.dir.join("forged.jpg");
    let refused = h.refused(
        "export.jpeg",
        with_envelope(json!({"asset_id":asset,"destination":destination})),
    );
    assert_eq!(refused.code, "validation");
    assert!(!destination.exists());
    assert_eq!(fs::read(&original).unwrap(), original_bytes);
}

/// What the stub tile service does with the next stream it is asked for.
#[derive(Clone)]
enum Behaviour {
    /// Draw the output stage in bands of this many rows; with `stop`, end the stream after that
    /// many bands, naming why the GPU cannot go on or with an error of its own.
    Bands {
        rows: u32,
        stop: Option<(usize, Stop)>,
    },
    /// Refuse the stream at once, for this reason.
    Refuse(TileFallback),
}

#[derive(Clone)]
enum Stop {
    Fallback(TileFallback),
    Error(Error),
}

/// A tile service standing in for the desktop's GPU worker. On a thread of its own it renders an
/// export's output stage with the reference renderer, each code's lowest bit flipped so a file it
/// drew is told apart from the reference renderer's ([`marked`]), and hands the stage over in
/// bands, checking the stream's cancellation before each and passing [`Self::gate`] before the band
/// [`Self::hold_before`] names; or it does what its [`Behaviour`] says.
struct StubTiles {
    behaviour: Mutex<Behaviour>,
    gate: Arc<Gate>,
    hold_before: Mutex<Option<usize>>,
    /// Streams asked for, and bands sent.
    streams: AtomicUsize,
    sent: Arc<AtomicUsize>,
}

impl StubTiles {
    fn new(behaviour: Behaviour) -> Arc<Self> {
        Arc::new(Self {
            behaviour: Mutex::new(behaviour),
            gate: Arc::new(Gate::new()),
            hold_before: Mutex::new(None),
            streams: AtomicUsize::new(0),
            sent: Arc::new(AtomicUsize::new(0)),
        })
    }

    fn behave(&self, behaviour: Behaviour) {
        *self.behaviour.lock().unwrap() = behaviour;
    }

    /// Hold the next streams before band `band`, at the shut gate.
    fn hold_before(&self, band: usize) {
        self.gate.shut();
        *self.hold_before.lock().unwrap() = Some(band);
    }

    fn streams(&self) -> usize {
        self.streams.load(Ordering::SeqCst)
    }

    fn sent(&self) -> usize {
        self.sent.load(Ordering::SeqCst)
    }
}

impl TileService for StubTiles {
    fn status(&self) -> TileStatus {
        TileStatus::Gpu
    }

    fn submit(&self, call: TileCall) {
        call.refuse(Error::internal("the stub reads no pixels"));
    }

    fn disconnect(&self, _: ClientId) {}

    fn stream(&self, evaluation: &Evaluation, cancel: &Cancel) -> Result<BandStream, TileFallback> {
        self.streams.fetch_add(1, Ordering::SeqCst);
        let (rows, stop) = match self.behaviour.lock().unwrap().clone() {
            Behaviour::Refuse(reason) => return Err(reason),
            Behaviour::Bands { rows, stop } => (rows, stop),
        };
        let identity = evaluation.identity().expect("the export's identity");
        let width = identity.width;
        let (sender, stream) = BandStream::channel(width, identity.height, Answered::gpu());
        let (evaluation, cancel) = (evaluation.clone(), cancel.clone());
        let (gate, hold, sent) = (
            Arc::clone(&self.gate),
            *self.hold_before.lock().unwrap(),
            Arc::clone(&self.sent),
        );
        std::thread::spawn(move || {
            let frame = match evaluation
                .exact(&Cancel::never())
                .and_then(|render| render.frame(SnapshotId::new()))
            {
                Ok(frame) => marked(&frame),
                Err(error) => {
                    sender.send(Err(error));
                    return;
                }
            };
            drop(evaluation);
            let stride = width as usize * 4;
            for (index, rgba) in frame.rgba.chunks(rows as usize * stride).enumerate() {
                if let Some((after, stop)) = &stop
                    && index == *after
                {
                    match stop {
                        Stop::Fallback(fallback) => sender.fall_back(fallback.clone()),
                        Stop::Error(error) => sender.send(Err(error.clone())),
                    };
                    return;
                }
                if hold == Some(index) {
                    gate.pass();
                }
                if let Err(cancelled) = cancel.check() {
                    sender.send(Err(cancelled));
                    return;
                }
                let band = Band {
                    y0: index as u32 * rows,
                    rows: (rgba.len() / stride) as u32,
                    rgba: rgba.to_vec(),
                };
                if !sender.send(Ok(band)) {
                    return;
                }
                sent.fetch_add(1, Ordering::SeqCst);
            }
        });
        Ok(stream)
    }

    fn stop(&self) {}
}

/// `frame` with every code's lowest bit flipped: what the stub draws for it, so a file it drew is
/// never the reference renderer's.
fn marked(frame: &crate::Raster) -> crate::Raster {
    let rgba: Vec<u8> = frame
        .rgba
        .chunks_exact(4)
        .flat_map(|pixel| [pixel[0] ^ 1, pixel[1] ^ 1, pixel[2] ^ 1, 255])
        .collect();
    crate::Raster {
        rgba: Arc::new(rgba),
        ..frame.clone()
    }
}

/// The names in `dir` that are temporary export files.
fn staged_files(dir: &Path) -> Vec<String> {
    listing(dir)
        .into_iter()
        .filter(|name| name.starts_with('.'))
        .collect()
}

/// A stream feeds the encoder its bands in order: the file is exactly the encoding of the frame the
/// provider drew, in bands of seven rows that never meet the encoder's strips of sixteen, whatever
/// metadata it carries, and its result names the GPU. While it is encoding its progress is the
/// share of the rows encoded. The original is unchanged.
#[test]
fn a_stream_feeds_the_encoder_in_order_and_names_the_gpu() {
    let stub = StubTiles::new(Behaviour::Bands {
        rows: 7,
        stop: None,
    });
    let mut harness = Harness::with_tiles("stream", stub.clone());
    let state = harness.import("stream.jpg");
    let asset = state["asset"]["id"].clone();
    let original = harness.dir.join("stream.jpg");
    let original_digest = digest(&original);
    let entry = harness.expose(&asset, 0.6);
    let out = destinations(&harness);

    // Held before its sixth band, the export has encoded the rows it has: 35 of them, two whole
    // strips, the second's start reported as the rows encoded before it over the stage's height.
    stub.hold_before(5);
    let held = harness.export(json!({"asset_id": asset, "destination": out.join("gpu.jpg")}));
    stub.gate.wait_reached(1, "the stub's sixth band");
    let encoding = luxforge_testbase::wait_for("the encoder's progress", || {
        let read = harness.ok("job.read", json!({"job_id": held["job_id"]}));
        (read["progress"]["fraction"].as_f64().unwrap_or(0.0) > 0.0).then_some(read)
    });
    assert_eq!(encoding["status"], "running", "{encoding}");
    assert_eq!(encoding["progress"]["message"], "encoding", "{encoding}");
    let fraction = encoding["progress"]["fraction"].as_f64().unwrap();
    assert!(
        (fraction - 16.0 / 320.0).abs() < 1e-6,
        "16 of 320 rows encoded: {fraction}"
    );
    assert_eq!(staged_files(&out).len(), 1, "one temporary file");
    *stub.hold_before.lock().unwrap() = None;
    stub.gate.open();
    let read = harness.settle(&held["job_id"]);
    assert_eq!(read["status"], "ready", "{read}");
    let bytes = fs::metadata(out.join("gpu.jpg")).unwrap().len();
    assert_eq!(
        read["result"],
        json!({
            "path": out.join("gpu.jpg"), "bytes": bytes, "width": 480, "height": 320,
            "metadata": [], "renderer": {"record": "gpu", "reason": null},
        })
    );
    let kept = harness.export(json!({
        "asset_id": asset, "destination": out.join("kept.jpg"), "keep_metadata": true,
    }));
    let read = harness.settle(&kept["job_id"]);
    assert_eq!(read["status"], "ready", "{read}");
    assert_eq!(read["result"]["renderer"]["record"], "gpu");
    assert!(has_app1(&fs::read(out.join("kept.jpg")).unwrap()));
    assert_eq!(stub.streams(), 2);
    assert_eq!(
        listing(&out),
        ["gpu.jpg", "kept.jpg"],
        "nothing else written"
    );
    harness.stop();
    let frame = reference(&harness.catalog, &asset, &entry);
    assert_encodes(&out.join("gpu.jpg"), &marked(&frame), "the stub's bands");
    assert_eq!(digest(&original), original_digest, "the original");
}

/// `reference: true` never asks the service for a stream: the reference renderer renders the file,
/// which is the encoding of its frame, and the result names it as requested.
#[test]
fn reference_true_never_asks_the_service() {
    let stub = StubTiles::new(Behaviour::Bands {
        rows: 16,
        stop: None,
    });
    let mut harness = Harness::with_tiles("asked", stub.clone());
    let state = harness.import("asked.jpg");
    let asset = state["asset"]["id"].clone();
    let original = harness.dir.join("asked.jpg");
    let original_digest = digest(&original);
    let entry = harness.expose(&asset, 0.4);
    let out = destinations(&harness);
    let asked = harness.export(json!({
        "asset_id": asset, "destination": out.join("asked.jpg"), "reference": true,
    }));
    let read = harness.settle(&asked["job_id"]);
    assert_eq!(read["status"], "ready", "{read}");
    assert_eq!(
        read["result"]["renderer"],
        json!({"record": "reference", "reason": "requested"})
    );
    assert_eq!(stub.streams(), 0, "the service was never asked");
    harness.stop();
    let frame = reference(&harness.catalog, &asset, &entry);
    assert_encodes(&out.join("asked.jpg"), &frame, "the reference export");
    assert_eq!(digest(&original), original_digest, "the original");
}

/// A stream the provider stops drawing part-way, a device lost, is dropped with its temporary file
/// and the export starts again on the reference renderer, from `rendering`: the file is the
/// reference's frame alone, and the result names why. A stream refused at once is rendered by the
/// reference for the reason given: a Dehaze light the store lacks, the budget, an adapter not named
/// yet. A stream that ends for an error of its own fails the export with it, writing nothing.
#[test]
fn a_stream_the_gpu_stops_drawing_is_rendered_again_by_the_reference_naming_why() {
    let lost = TileFallback::Unavailable(TileUnavailable::DeviceLost);
    let stub = StubTiles::new(Behaviour::Bands {
        rows: 16,
        stop: Some((2, Stop::Fallback(lost))),
    });
    let mut harness = Harness::with_tiles("stopped", stub.clone());
    let state = harness.import("stopped.jpg");
    let asset = state["asset"]["id"].clone();
    let original = harness.dir.join("stopped.jpg");
    let original_digest = digest(&original);
    let entry = harness.expose(&asset, -0.5);
    let out = destinations(&harness);
    let phases = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&phases);
    harness.owner.hold_exports(Some(Arc::new(move |phase| {
        recorded.lock().unwrap().push(phase)
    })));

    let export = |name: &str| {
        phases.lock().unwrap().clear();
        let job = harness.export(json!({"asset_id": asset, "destination": out.join(name)}));
        let read = harness.settle(&job["job_id"]);
        (read, phases.lock().unwrap().clone())
    };
    let (read, phases_of) = export("lost.jpg");
    assert_eq!(read["status"], "ready", "{read}");
    assert_eq!(
        read["result"]["renderer"],
        json!({"record": "reference", "reason": "device-lost"})
    );
    assert_eq!(
        phases_of,
        ["rendering", "encoding", "rendering", "encoding", "writing"]
    );
    assert_eq!(stub.sent(), 2, "two bands before the device was lost");

    for (refusal, reason) in [
        (
            TileFallback::Plan(GpuFallback::Unplannable(
                "a plan the planner cannot cut".into(),
            )),
            "unplannable",
        ),
        (
            TileFallback::Budget {
                requested: 2 << 30,
                budget: 768 << 20,
            },
            "tiles-budget",
        ),
        (
            TileFallback::Unavailable(TileUnavailable::Pending),
            "surface-pending",
        ),
    ] {
        stub.behave(Behaviour::Refuse(refusal));
        let (read, phases_of) = export(&format!("{reason}.jpg"));
        assert_eq!(read["status"], "ready", "{read}");
        assert_eq!(
            read["result"]["renderer"],
            json!({"record": "reference", "reason": reason})
        );
        assert_eq!(phases_of, ["rendering", "encoding", "writing"], "{reason}");
    }

    stub.behave(Behaviour::Bands {
        rows: 16,
        stop: Some((1, Stop::Error(Error::internal("the stub's own failure")))),
    });
    let (read, _) = export("failed.jpg");
    assert_eq!(read["status"], "failed", "{read}");
    assert_eq!(read["error"]["code"], "internal", "{read}");
    assert!(
        read["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("the stub's own failure")),
        "{read}"
    );
    assert_eq!(
        listing(&out),
        [
            "lost.jpg",
            "surface-pending.jpg",
            "tiles-budget.jpg",
            "unplannable.jpg"
        ],
        "no temporary file and nothing of the failed export"
    );
    harness.owner.hold_exports(None);
    harness.stop();
    let frame = reference(&harness.catalog, &asset, &entry);
    for name in [
        "lost.jpg",
        "unplannable.jpg",
        "tiles-budget.jpg",
        "surface-pending.jpg",
    ] {
        assert_encodes(&out.join(name), &frame, name);
    }
    assert_eq!(digest(&original), original_digest, "the original");
}

/// A cancel while the stream is held between its tiles stops it there: the provider sends nothing
/// more, the export ends cancelled, and its temporary file is removed. The original is unchanged.
#[test]
fn a_cancel_between_tiles_writes_nothing_more_and_removes_the_staged_file() {
    let stub = StubTiles::new(Behaviour::Bands {
        rows: 16,
        stop: None,
    });
    let harness = Harness::with_tiles("cancelled-stream", stub.clone());
    let state = harness.import("cancelled.jpg");
    let asset = state["asset"]["id"].clone();
    let original = harness.dir.join("cancelled.jpg");
    let original_digest = digest(&original);
    harness.expose(&asset, 0.3);
    let out = destinations(&harness);
    stub.hold_before(2);
    let job = harness.export(json!({"asset_id": asset, "destination": out.join("never.jpg")}));
    stub.gate.wait_reached(1, "the stub's third band");
    assert_eq!(staged_files(&out).len(), 1, "staged while it streams");
    let requested = harness.ok("job.cancel", json!({"job_id": job["job_id"]}));
    assert_eq!(requested["status"], "running", "it stops at its next tile");
    stub.gate.open();
    let read = harness.settle(&job["job_id"]);
    assert_eq!(read["status"], "cancelled", "{read}");
    assert_eq!(read["error"]["message"], "the export was cancelled");
    assert_eq!(stub.sent(), 2, "nothing drawn after the cancel");
    assert!(listing(&out).is_empty(), "{:?}", listing(&out));
    assert_eq!(digest(&original), original_digest, "the original");
}

/// The same held command used by the desktop sleeps without rereads, releases on cancellation,
/// and answers only after a terminal outcome is readable. A single executor thread can hold sixteen
/// waits while the owner serves unrelated commands.
#[test]
fn job_wait_is_event_driven_bounded_cancel_safe_and_publishes_final_results() {
    use std::{
        future::Future,
        task::{Context, Poll, Waker},
    };
    let h = Harness::start("job-wait");
    let asset = h.import("input.jpg")["asset"]["id"].clone();
    let (reached, release) = h.hold_at("writing");
    let accepted = h.export(json!({"asset_id":asset,"destination":h.dir.join("output.jpg")}));
    reached.recv_timeout(luxforge_testbase::HANG).unwrap();
    let job = accepted["job_id"].clone();
    let first = h.ok("job.wait", json!({"job_id":job}));
    assert_eq!(first["job"]["status"], "running");
    let token = first["change"].clone();
    let request = || ApiRequest {
        id: "monitor".into(),
        method: "job.wait".into(),
        params: json!({"job_id":job,"after":token}),
        token: None,
    };
    let mut context = Context::from_waker(Waker::noop());
    let mut held: Vec<_> = (0..MAX_CLIENT_JOB_WAITERS)
        .map(|_| Box::pin(h.owner.call_async(h.client, request())))
        .collect();
    for wait in &mut held {
        assert!(wait.as_mut().poll(&mut context).is_pending());
    }
    assert_eq!(h.owner.job_monitor_stats().held, MAX_CLIENT_JOB_WAITERS);
    let before = h.owner.job_monitor_stats();
    let _ = h.ok("catalog.info", json!({}));
    assert_eq!(
        h.owner.job_monitor_stats().requests,
        before.requests,
        "other commands do not read monitored jobs"
    );
    assert_eq!(
        h.owner.job_monitor_stats().replies,
        before.replies,
        "quiet jobs have no timer-driven replies"
    );
    let mut excess = Box::pin(h.owner.call_async(h.client, request()));
    assert!(excess.as_mut().poll(&mut context).is_pending());
    let _ = h.owner.job_monitor_stats();
    let Poll::Ready(Ok(refused)) = excess.as_mut().poll(&mut context) else {
        panic!("the waiter limit answers immediately");
    };
    assert_eq!(refused.error.unwrap().code, "resource-limit");
    let second = h.owner.register();
    for _ in 0..MAX_CLIENT_JOB_WAITERS {
        let mut wait = Box::pin(h.owner.call_async(second, request()));
        assert!(wait.as_mut().poll(&mut context).is_pending());
        held.push(wait);
    }
    assert_eq!(h.owner.job_monitor_stats().held, MAX_JOB_WAITERS);
    let third = h.owner.register();
    let mut global_excess = Box::pin(h.owner.call_async(third, request()));
    assert!(global_excess.as_mut().poll(&mut context).is_pending());
    let _ = h.owner.job_monitor_stats();
    let Poll::Ready(Ok(refused)) = global_excess.as_mut().poll(&mut context) else {
        panic!("the global waiter limit answers immediately");
    };
    assert_eq!(refused.error.unwrap().code, "resource-limit");
    held.clear();
    assert_eq!(
        h.owner.job_monitor_stats().held,
        0,
        "dropping futures releases interests while the job runs"
    );
    let queued = h.export(json!({"asset_id":asset,"destination":h.dir.join("queued.jpg")}));
    let queued_id = queued["job_id"].clone();
    let observed = h.ok("job.wait", json!({"job_id":queued_id}));
    assert_eq!(observed["job"]["status"], "queued");
    let mut cancelled = Box::pin(h.owner.call_async(
        h.client,
        ApiRequest {
            id: "queued-cancel".into(),
            method: "job.wait".into(),
            params: json!({"job_id":queued_id,"after":observed["change"]}),
            token: None,
        },
    ));
    assert!(cancelled.as_mut().poll(&mut context).is_pending());
    assert_eq!(h.owner.job_monitor_stats().held, 1);
    h.ok("job.cancel", json!({"job_id":queued_id}));
    let _ = h.owner.job_monitor_stats();
    let Poll::Ready(Ok(response)) = cancelled.as_mut().poll(&mut context) else {
        panic!("queued cancellation wakes its waiter");
    };
    assert_eq!(response.result.unwrap()["job"]["status"], "cancelled");
    assert!(!h.dir.join("queued.jpg").exists());
    assert_eq!(
        h.ok(
            "job.wait",
            json!({"job_id":job,"after":token,"timeout_ms":0})
        )["job"]["status"],
        "running",
        "an explicit zero timeout observes immediately"
    );
    let mut final_wait = Box::pin(h.owner.call_async(h.client, request()));
    assert!(final_wait.as_mut().poll(&mut context).is_pending());
    assert_eq!(h.owner.job_monitor_stats().held, 1);
    release.send(()).unwrap();
    let ready = h.settle(&job);
    assert_eq!(ready["status"], "ready");
    let _ = h.owner.job_monitor_stats();
    let Poll::Ready(Ok(response)) = final_wait.as_mut().poll(&mut context) else {
        panic!("terminal result wakes its held command");
    };
    assert_eq!(response.result.unwrap()["job"], ready);
    assert_eq!(
        h.ok("job.wait", json!({"job_id":job,"after":token}))["job"],
        ready,
        "completion before rearm is immediate"
    );
}
