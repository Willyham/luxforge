//! The 100% region through the catalog owner: `preview.region` and `job.read` over
//! [`OwnerHandle::call`] for a file and for a developed photograph's original, the answer against
//! the rectangle cut from the file's upright decode, the frame, latest wins per client with the
//! worker held at a gate, two clients in turn, the answer file replaced and removed, a cancel
//! waking a wait for the development, the queue's bound, the refusals and every error's own kind,
//! and, with the supplied RAW files, both paths on real cameras.
use super::REGION_QUEUE_CAPACITY;
use crate::{
    ApiRequest, ApiResponse, AssetId, EditorService, EntryId, SourceTag,
    api::owner::{ClientId, OwnerHandle},
    catalog_types::{FileId, FileSignature, HeaderState},
    index::{IndexDb, index_dir},
    open_source_bytes,
    previews::{
        preview_cache::{add_file, broken_jpeg, synthetic_dng},
        region::{self, Developed, FrameSize},
    },
};
use luxforge_testbase::{Gate, paths, wait_for, wait_until};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    thread::{self, JoinHandle},
};

/// An owner over a new catalog, the index beside it for the test to list files in, and a client.
struct Setup {
    root: PathBuf,
    owner: OwnerHandle,
    join: Option<JoinHandle<()>>,
    index: IndexDb,
    client: ClientId,
}

impl Setup {
    fn new(name: &str) -> Self {
        Self::with_photo(name, None).0
    }

    /// The setup, with the fixture `photo` (a file of `fixtures/s0`) copied into its folder and
    /// developed into the catalog before the owner starts: its asset and current entry.
    fn with_photo(name: &str, photo: Option<&str>) -> (Self, Option<(AssetId, EntryId, PathBuf)>) {
        let root = paths::temp_dir(name);
        let catalog = root.join("catalog.sqlite");
        let mut service = EditorService::open(&catalog).unwrap();
        let developed = photo.map(|photo| {
            let path = root.join(photo);
            fs::copy(paths::fixture(&format!("s0/{photo}")), &path).unwrap();
            let state = service.import(&path).unwrap();
            (state.asset.id, state.current_entry.id, path)
        });
        let catalog_id = service.catalog_id().to_owned();
        drop(service);
        let (index, _) = IndexDb::open(&index_dir(&catalog), &catalog_id).unwrap();
        let (owner, join) = OwnerHandle::start(&catalog).unwrap();
        let client = owner.register();
        (
            Self {
                root,
                owner,
                join: Some(join),
                index,
                client,
            },
            developed,
        )
    }

    /// `bytes` written as `name` in the scratch folder and listed in the index as `kind`.
    fn file(&mut self, name: &str, bytes: &[u8], kind: SourceTag) -> (FileId, PathBuf) {
        let path = self.root.join(name);
        fs::write(&path, bytes).unwrap();
        let file = add_file(&mut self.index, &path, kind, HeaderState::Pending);
        (file, path)
    }

    /// A fixture of `fixtures/s0` copied in and listed.
    fn fixture(&mut self, name: &str) -> (FileId, PathBuf) {
        let bytes = fs::read(paths::fixture(&format!("s0/{name}"))).unwrap();
        self.file(name, &bytes, SourceTag::Jpeg)
    }

    fn call(&self, client: ClientId, method: &str, params: Value) -> ApiResponse {
        self.owner
            .call(
                client,
                ApiRequest {
                    id: method.into(),
                    method: method.into(),
                    params,
                    token: None,
                },
            )
            .expect("the owner answered")
    }

    fn ok_for(&self, client: ClientId, method: &str, params: Value) -> Value {
        let response = self.call(client, method, params);
        assert!(response.error.is_none(), "{method}: {:?}", response.error);
        response.result.expect("a result")
    }

    fn ok(&self, method: &str, params: Value) -> Value {
        self.ok_for(self.client, method, params)
    }

    fn failure_for(&self, client: ClientId, method: &str, params: Value) -> String {
        self.call(client, method, params)
            .error
            .unwrap_or_else(|| panic!("{method} was expected to fail"))
            .code
    }

    /// `client`'s region of `item`, the job it started.
    fn region(
        &self,
        client: ClientId,
        item: Value,
        rect: [u32; 4],
        frame: Option<[u32; 2]>,
    ) -> Value {
        let [x, y, width, height] = rect;
        let mut params =
            json!({"item": item, "rect": {"x": x, "y": y, "width": width, "height": height}});
        if let Some([width, height]) = frame {
            params["frame"] = json!({"width": width, "height": height});
        }
        let started = self.ok_for(client, "preview.region", params);
        assert_eq!(started["deduplicated"], false, "{started}");
        started["job_id"].clone()
    }

    /// The job once it is no longer queued or running.
    fn settled(&self, job: &Value) -> Value {
        wait_for("the job to end", || {
            let record = self.ok("job.read", json!({"job_id": job}));
            (!matches!(record["status"].as_str(), Some("queued" | "running"))).then_some(record)
        })
    }

    /// The answer of a region that ends ready.
    fn answered(&self, job: &Value) -> Value {
        let record = self.settled(job);
        assert_eq!(record["status"], "ready", "{record}");
        assert_eq!(record["kind"], "preview-region");
        record["result"].clone()
    }

    fn regions_dir(&self) -> PathBuf {
        self.root.join("catalog.index/previews/regions")
    }

    /// The answer files there now, by name.
    fn answer_files(&self) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(self.regions_dir())
            .map(|entries| {
                entries
                    .flatten()
                    .map(|entry| entry.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    }
}

impl Drop for Setup {
    fn drop(&mut self) {
        self.owner.hold_regions(None);
        self.owner.stop();
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn file_item(file: FileId) -> Value {
    json!({"kind": "file", "file_id": file})
}

/// `[x, y, width, height]` cut from an upright RGBA8 frame `width` pixels wide.
fn cut(rgba: &[u8], width: u32, rect: [u32; 4]) -> Vec<u8> {
    let [x, y, w, h] = rect;
    let mut out = Vec::new();
    for row in y..y + h {
        let start = ((row * width + x) * 4) as usize;
        out.extend_from_slice(&rgba[start..start + w as usize * 4]);
    }
    out
}

/// The answer JPEG `rect` of the JPEG at `path` must be: that rectangle cut from the file's whole
/// upright decode, independently of the region decode, encoded as a region's answer is.
fn expected_answer(path: &Path, rect: [u32; 4]) -> Vec<u8> {
    let upright = open_source_bytes(fs::read(path).unwrap()).unwrap();
    region::encode_rgba(rect[2], rect[3], &cut(&upright.rgba, upright.width, rect)).unwrap()
}

fn rect_json(rect: [u32; 4]) -> Value {
    json!({"x": rect[0], "y": rect[1], "width": rect[2], "height": rect[3]})
}

/// A JPEG file's region is the rectangle cut from its upright decode, labelled `embedded`, with
/// the frame it is in; a rectangle against a named frame keeps its size, its centre mapped into the
/// file's frame, and one running past the frame is clamped. The file is only read.
#[test]
fn preview_region_owner_answers_a_jpeg_region_cut_from_its_upright_decode() {
    let mut setup = Setup::new("preview-region-owner-jpeg");
    let (file, path) = setup.fixture("orientation-6.jpg");
    let bytes = fs::read(&path).unwrap();
    let upright = open_source_bytes(bytes.clone()).unwrap();
    let (w, h) = (upright.width, upright.height);

    let rect = [13, 7, 37, 29];
    let answer = setup.answered(&setup.region(setup.client, file_item(file), rect, None));
    assert_eq!(answer["origin"], "embedded");
    assert_eq!(answer["item"], file_item(file));
    assert_eq!(answer["rect"], rect_json(rect));
    assert_eq!(answer["frame"], json!({"width": w, "height": h}));
    assert_eq!(
        (answer["width"].clone(), answer["height"].clone()),
        (json!(37), json!(29))
    );
    let answer_path = PathBuf::from(answer["path"].as_str().unwrap());
    assert!(
        answer_path.starts_with(fs::canonicalize(setup.regions_dir()).unwrap()),
        "{answer_path:?}"
    );
    assert!(fs::read(&answer_path).unwrap() == expected_answer(&path, rect));

    // Against a frame twice the size: the centre (110, 65) of the named frame is (55, 32.5) here.
    let answer = setup.answered(&setup.region(
        setup.client,
        file_item(file),
        [100, 60, 20, 10],
        Some([2 * w, 2 * h]),
    ));
    let mapped = [45, 27, 20, 10];
    assert_eq!(answer["rect"], rect_json(mapped));
    assert!(fs::read(answer["path"].as_str().unwrap()).unwrap() == expected_answer(&path, mapped));

    let answer =
        setup.answered(&setup.region(setup.client, file_item(file), [w - 10, h - 5, 64, 64], None));
    assert_eq!(answer["rect"], rect_json([w - 10, h - 5, 10, 5]), "clamped");
    assert_eq!(fs::read(&path).unwrap(), bytes, "the file is only read");
}

/// A client's next region cancels its previous one, which the job table records `cancelled`,
/// while the worker holds it at a gate; the running region is on the activity board as "Checking
/// focus" with its job; the cancelled one writes nothing.
#[test]
fn preview_region_owner_latest_wins_per_client() {
    let mut setup = Setup::new("preview-region-owner-latest");
    let (file, _) = setup.fixture("orientation-1.jpg");
    let gate = Arc::new(Gate::new());
    gate.shut();
    setup.owner.hold_regions(Some(gate.clone()));
    let first = setup.region(setup.client, file_item(file), [0, 0, 16, 16], None);
    gate.wait_reached(1, "the first region");
    assert_eq!(
        setup.ok("job.read", json!({"job_id": first}))["status"],
        "running"
    );
    let board = setup.ok("activity.list", json!({}));
    let entry = board["active"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["kind"] == "preview.region")
        .cloned()
        .unwrap_or_else(|| panic!("the region is on the board: {board}"));
    assert_eq!(entry["label"], "Checking focus");
    assert_eq!(entry["job_id"], first);

    let second = setup.region(setup.client, file_item(file), [8, 8, 16, 16], None);
    let cancelled = setup.ok("job.read", json!({"job_id": first}));
    assert_eq!(cancelled["status"], "cancelled", "{cancelled}");
    assert_eq!(cancelled["error"]["code"], "cancelled");
    assert_eq!(
        setup.ok("job.read", json!({"job_id": second}))["status"],
        "queued"
    );
    gate.open();
    let answer = setup.answered(&second);
    assert_eq!(answer["rect"], rect_json([8, 8, 16, 16]));
    assert_eq!(setup.settled(&first)["status"], "cancelled");
    let written = PathBuf::from(answer["path"].as_str().unwrap());
    let name = written.file_name().unwrap().to_string_lossy().into_owned();
    assert_eq!(
        setup.answer_files(),
        [name],
        "the cancelled region wrote nothing"
    );
}

/// Regions of two clients wait in turn and both answer, each with its own file.
#[test]
fn preview_region_owner_answers_two_clients_in_turn() {
    let mut setup = Setup::new("preview-region-owner-two");
    let (file, path) = setup.fixture("orientation-3.jpg");
    let other = setup.owner.register();
    let gate = Arc::new(Gate::new());
    gate.shut();
    setup.owner.hold_regions(Some(gate.clone()));
    let mine = setup.region(setup.client, file_item(file), [1, 2, 30, 20], None);
    gate.wait_reached(1, "the first region");
    let theirs = setup.region(other, file_item(file), [40, 30, 25, 25], None);
    assert_eq!(
        setup.ok("job.read", json!({"job_id": theirs}))["status"],
        "queued"
    );
    gate.open();
    let (mine, theirs) = (setup.answered(&mine), setup.answered(&theirs));
    assert_ne!(mine["path"], theirs["path"]);
    for (answer, rect) in [(&mine, [1, 2, 30, 20]), (&theirs, [40, 30, 25, 25])] {
        assert!(
            fs::read(answer["path"].as_str().unwrap()).unwrap() == expected_answer(&path, rect)
        );
    }
}

/// A client's answer file is valid until its next region: the next answer replaces it, removing
/// it, and disconnecting removes the last.
#[test]
fn preview_region_owner_replaces_the_answer_and_removes_it_on_disconnect() {
    let mut setup = Setup::new("preview-region-owner-answers");
    let (file, _) = setup.fixture("orientation-1.jpg");
    let leaving = setup.owner.register();
    let first = setup.region(leaving, file_item(file), [0, 0, 8, 8], None);
    let first = PathBuf::from(setup.answered(&first)["path"].as_str().unwrap());
    assert!(first.is_file());
    let second = setup.region(leaving, file_item(file), [8, 8, 8, 8], None);
    let second = PathBuf::from(setup.answered(&second)["path"].as_str().unwrap());
    assert_ne!(first, second);
    assert!(second.is_file());
    assert!(!first.exists(), "the previous answer is removed");
    let mine = setup.region(setup.client, file_item(file), [0, 0, 8, 8], None);
    let mine = PathBuf::from(setup.answered(&mine)["path"].as_str().unwrap());
    setup.owner.disconnect(leaving);
    wait_until("the leaving client's answer is removed", || {
        !second.exists()
    });
    assert!(mine.is_file(), "another client's answer stays");
}

/// `job.cancel` of a region waiting for the one development — another holds the process's slot —
/// wakes it: the job ends `cancelled` and the worker is free for the next region while the other
/// development still runs.
#[test]
fn preview_region_owner_cancel_wakes_a_development_waiter() {
    let mut setup = Setup::new("preview-region-owner-wake");
    let (raw, _) = setup.file(
        "H265.DNG",
        &synthetic_dng("DJI", "FC3411", 64, 48),
        SourceTag::Raw,
    );
    let (jpeg, _) = setup.fixture("orientation-1.jpg");
    let held = setup.root.join("held.raw");
    fs::write(&held, b"held").unwrap();
    let signature = FileSignature::of(&fs::metadata(&held).unwrap());
    let gate = Arc::new(Gate::new());
    gate.shut();
    let holder = {
        let gate = gate.clone();
        let held = held.clone();
        thread::spawn(move || {
            region::develop_in_slot(&held, &signature, |_, _| {
                gate.pass();
                let size = FrameSize {
                    width: 8,
                    height: 8,
                };
                Ok(Developed {
                    size,
                    rgba: Arc::new(vec![0; 8 * 8 * 4]),
                })
            })
        })
    };
    gate.wait_reached(1, "the held development");
    let job = setup.region(setup.client, file_item(raw), [0, 0, 8, 8], None);
    wait_until("the region waits for the development", || {
        region::development_waiters() >= 1
    });
    let cancelled = setup.ok("job.cancel", json!({"job_id": job}));
    assert_eq!(cancelled["status"], "cancelled", "{cancelled}");
    let other = setup.owner.register();
    let next = setup.region(other, file_item(jpeg), [0, 0, 8, 8], None);
    assert_eq!(setup.answered(&next)["origin"], "embedded");
    assert!(gate.holding(), "the other development still runs");
    gate.open();
    holder.join().unwrap().unwrap();
    region::release_development();
}

/// At most [`REGION_QUEUE_CAPACITY`] regions wait: one past it is `resource-limit`, while a
/// waiting client's next region replaces its own.
#[test]
fn preview_region_owner_refuses_past_the_queue_bound() {
    let mut setup = Setup::new("preview-region-owner-bound");
    let (file, _) = setup.fixture("orientation-1.jpg");
    let gate = Arc::new(Gate::new());
    gate.shut();
    setup.owner.hold_regions(Some(gate.clone()));
    let running = setup.region(setup.client, file_item(file), [0, 0, 4, 4], None);
    gate.wait_reached(1, "the running region");
    let clients: Vec<ClientId> = (0..=REGION_QUEUE_CAPACITY)
        .map(|_| setup.owner.register())
        .collect();
    let waiting: Vec<Value> = clients[..REGION_QUEUE_CAPACITY]
        .iter()
        .map(|client| setup.region(*client, file_item(file), [0, 0, 4, 4], None))
        .collect();
    let refused = json!({"item": file_item(file), "rect": rect_json([0, 0, 4, 4])});
    assert_eq!(
        setup.failure_for(clients[REGION_QUEUE_CAPACITY], "preview.region", refused),
        "resource-limit"
    );
    let replaced = setup.region(clients[0], file_item(file), [4, 4, 4, 4], None);
    assert_eq!(
        setup.ok("job.read", json!({"job_id": waiting[0]}))["status"],
        "cancelled"
    );
    gate.open();
    assert_eq!(setup.answered(&running)["rect"], rect_json([0, 0, 4, 4]));
    assert_eq!(setup.answered(&replaced)["rect"], rect_json([4, 4, 4, 4]));
    for job in &waiting[1..] {
        setup.answered(job);
    }
    assert_eq!(REGION_QUEUE_CAPACITY, 9);
}

/// A file changed since it was indexed is `source-unavailable`, and so is one that is gone.
#[test]
fn preview_region_owner_refuses_a_changed_or_missing_file() {
    let mut setup = Setup::new("preview-region-owner-changed");
    let (file, path) = setup.fixture("orientation-1.jpg");
    setup.answered(&setup.region(setup.client, file_item(file), [0, 0, 8, 8], None));
    let mut bytes = fs::read(&path).unwrap();
    bytes.extend_from_slice(b"changed");
    fs::write(&path, &bytes).unwrap();
    let record = setup.settled(&setup.region(setup.client, file_item(file), [0, 0, 8, 8], None));
    assert_eq!(record["status"], "failed");
    assert_eq!(record["error"]["code"], "source-unavailable");
    fs::remove_file(&path).unwrap();
    let record = setup.settled(&setup.region(setup.client, file_item(file), [0, 0, 8, 8], None));
    assert_eq!(record["error"]["code"], "source-unavailable");
}

/// A developed photograph's region is its original's, answered exactly as the file's, at its
/// current entry or a named one of its own; an entry of no such photograph, an unknown photograph
/// and an original that is gone are refused.
#[test]
fn preview_region_owner_answers_a_photograph_as_its_original() {
    let (setup, developed) =
        Setup::with_photo("preview-region-owner-photo", Some("orientation-8.jpg"));
    let (asset, entry, path) = developed.unwrap();
    let rect = [5, 9, 33, 21];
    let item = json!({"kind": "photo", "asset_id": asset});
    let job = setup.region(setup.client, item.clone(), rect, None);
    let record = setup.settled(&job);
    assert_eq!(record["asset_id"], json!(asset), "{record}");
    let answer = record["result"].clone();
    assert_eq!(answer["origin"], "embedded");
    assert_eq!(answer["item"], item);
    assert_eq!(answer["rect"], rect_json(rect));
    assert!(fs::read(answer["path"].as_str().unwrap()).unwrap() == expected_answer(&path, rect));

    let named = json!({"kind": "photo", "asset_id": asset, "entry_id": entry});
    assert_eq!(
        setup.answered(&setup.region(setup.client, named, rect, None))["origin"],
        "embedded"
    );
    let params = |item: Value| json!({"item": item, "rect": rect_json(rect)});
    assert_eq!(
        setup.failure_for(
            setup.client,
            "preview.region",
            params(json!({"kind": "photo", "asset_id": asset, "entry_id": EntryId::new()}))
        ),
        "validation"
    );
    assert_eq!(
        setup.failure_for(
            setup.client,
            "preview.region",
            params(json!({"kind": "photo", "asset_id": AssetId::new()}))
        ),
        "validation"
    );
    fs::rename(&path, path.with_extension("moved")).unwrap();
    let record = setup.settled(&setup.region(setup.client, item, rect, None));
    assert_eq!(record["error"]["code"], "source-unavailable", "{record}");
}

/// Every error keeps its own kind: a JPEG cut short is `invalid-input` for a region in the rows it
/// lacks (a region of the rows it holds decodes, since only those are read), as is a file that is
/// no JPEG, and `preview.read` of it is `invalid-input` too; a rectangle of no pixels, a frame of
/// no pixels and a file the index does not hold are `validation` and one past the region limit
/// `resource-limit`, at once; a rectangle starting outside its frame fails its job with
/// `validation`.
#[test]
fn preview_region_owner_keeps_each_errors_kind() {
    let mut setup = Setup::new("preview-region-owner-errors");
    let (broken, _) = setup.file("BROKEN.JPG", &broken_jpeg(), SourceTag::Jpeg);
    let (not_jpeg, _) = setup.file("NOT.JPG", b"not a JPEG\n", SourceTag::Jpeg);
    for (file, rect) in [(broken, [0, 419, 8, 8]), (not_jpeg, [0, 0, 8, 8])] {
        let record = setup.settled(&setup.region(setup.client, file_item(file), rect, None));
        assert_eq!(record["status"], "failed", "{record}");
        assert_eq!(record["error"]["code"], "invalid-input", "{record}");
        let read = setup.ok(
            "preview.read",
            json!({"item": file_item(file), "tier": "loupe"}),
        );
        let record = setup.settled(&read["job_id"]);
        assert_eq!(record["error"]["code"], "invalid-input", "{record}");
    }
    assert_eq!(
        setup.answered(&setup.region(setup.client, file_item(broken), [0, 0, 8, 8], None))["origin"],
        "embedded"
    );

    let (file, _) = setup.fixture("orientation-1.jpg");
    let refused = |item: Value, rect: [u32; 4], frame: Option<[u32; 2]>| {
        let mut params = json!({"item": item, "rect": rect_json(rect)});
        if let Some([width, height]) = frame {
            params["frame"] = json!({"width": width, "height": height});
        }
        setup.failure_for(setup.client, "preview.region", params)
    };
    assert_eq!(refused(file_item(file), [0, 0, 0, 8], None), "validation");
    assert_eq!(
        refused(file_item(file), [0, 0, 8, 8], Some([0, 8])),
        "validation"
    );
    assert_eq!(
        refused(file_item(file), [0, 0, 4000, 4000], None),
        "resource-limit"
    );
    assert_eq!(
        refused(file_item(FileId(999_999)), [0, 0, 8, 8], None),
        "validation"
    );
    let record = setup.settled(&setup.region(setup.client, file_item(file), [9000, 0, 8, 8], None));
    assert_eq!(record["error"]["code"], "validation", "{record}");
}

fn sha256(path: &Path) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(fs::read(path).unwrap())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// The supplied RAW files through `preview.region`, read in place and never written: the Z6's
/// full-size embedded preview answers `embedded`; the X100VI's reduced preview and the Air 2S's
/// 960 px one answer from a development, `developed`, whose next region of the same frame comes
/// from the kept development. Run in release:
///
/// ```sh
/// LUXFORGE_RAW_OWNER_DIR=/path/to/raw cargo test --release -p luxforge-core --lib \
///   preview_region_owner -- --ignored --nocapture
/// ```
#[test]
#[ignore = "needs LUXFORGE_RAW_OWNER_DIR: the owner's RAW files, outside Git; run in release"]
fn preview_region_owner_supplied_raws() {
    let dir = PathBuf::from(
        std::env::var_os("LUXFORGE_RAW_OWNER_DIR")
            .expect("LUXFORGE_RAW_OWNER_DIR names the owner's RAW folder"),
    );
    let mut setup = Setup::new("preview-region-owner-raws");
    for (name, origin) in [
        ("nikon_z6.NEF", "embedded"),
        ("fujifilm_x100vi.RAF", "developed"),
        ("mavic_air_2s.DNG", "developed"),
    ] {
        let path = dir.join(name);
        let before = sha256(&path);
        let file = add_file(
            &mut setup.index,
            &path,
            SourceTag::Raw,
            HeaderState::Pending,
        );
        let answer = setup.answered(&setup.region(
            setup.client,
            file_item(file),
            [0, 0, 1, 1],
            Some([2, 2]),
        ));
        assert_eq!(answer["origin"], origin, "{name}: {answer}");
        let frame = [
            answer["frame"]["width"].as_u64().unwrap() as u32,
            answer["frame"]["height"].as_u64().unwrap() as u32,
        ];
        // A 512 px square at the frame's centre.
        let rect = [frame[0] / 2 - 256, frame[1] / 2 - 256, 512, 512];
        let centre = setup.answered(&setup.region(setup.client, file_item(file), rect, None));
        assert_eq!(centre["origin"], origin, "{name}");
        assert_eq!(centre["rect"], rect_json(rect), "{name}");
        assert_eq!(centre["frame"], answer["frame"], "{name}");
        let decoded = image::load_from_memory_with_format(
            &fs::read(centre["path"].as_str().unwrap()).unwrap(),
            image::ImageFormat::Jpeg,
        )
        .unwrap();
        assert_eq!((decoded.width(), decoded.height()), (512, 512), "{name}");
        assert_eq!(sha256(&path), before, "{name} is unchanged");
        println!("{name}: {origin}, a {} x {} frame", frame[0], frame[1]);
    }
    region::release_development();
}
