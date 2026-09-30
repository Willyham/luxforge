//! The preview lane through the catalog owner: `preview.read` and `job.read` over
//! [`OwnerHandle::call`], the grid's two stages, the lane's order with the workers held at a gate,
//! view jobs and their progress, `job.cancel` of queued and running work, the queue's bound, the
//! refusals, a file with no usable preview, and the waker.
use super::super::{ClientId, OwnerHandle};
use crate::{
    ApiRequest, ApiResponse, EditorService, SourceTag,
    catalog_types::{
        FileId, FileRecord, FileSignature, HeaderState, PreviewState, PreviewTier, VolumeId,
    },
    index::{IndexDb, index_dir, upsert_file},
    previews::preview_cache::{add_file, camera_jpeg, decoded, header},
};
use luxforge_testbase::{Gate, paths::temp_dir, wait_for, wait_until};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    thread::JoinHandle,
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
        let root = temp_dir(name);
        let catalog = root.join("catalog.sqlite");
        let catalog_id = EditorService::open(&catalog)
            .unwrap()
            .catalog_id()
            .to_owned();
        let (index, _) = IndexDb::open(&index_dir(&catalog), &catalog_id).unwrap();
        let (owner, join) = OwnerHandle::start(&catalog).unwrap();
        let client = owner.register();
        Self {
            root,
            owner,
            join: Some(join),
            index,
            client,
        }
    }

    /// A camera JPEG with an EXIF thumbnail, listed in the index.
    fn jpeg(&mut self, name: &str) -> FileId {
        let (bytes, offset, len) = camera_jpeg((640, 427), (160, 107), 1);
        let path = self.root.join(name);
        fs::write(&path, &bytes).unwrap();
        add_file(
            &mut self.index,
            &path,
            SourceTag::Jpeg,
            header(1, Some((offset, len, (160, 107)))),
        )
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

    fn ok(&self, method: &str, params: Value) -> Value {
        let response = self.call(self.client, method, params);
        assert!(response.error.is_none(), "{method}: {:?}", response.error);
        response.result.expect("a result")
    }

    fn failure(&self, method: &str, params: Value) -> String {
        self.call(self.client, method, params)
            .error
            .unwrap_or_else(|| panic!("{method} was expected to fail"))
            .code
    }

    fn read(&self, file: FileId, tier: &str, priority: &str) -> Value {
        self.ok(
            "preview.read",
            json!({"item": {"kind": "file", "file_id": file}, "tier": tier, "priority": priority}),
        )
    }

    /// The job once it is no longer queued or running.
    fn settled(&self, job: &Value) -> Value {
        wait_for("the job to end", || {
            let record = self.ok("job.read", json!({"job_id": job}));
            (!matches!(record["status"].as_str(), Some("queued" | "running"))).then_some(record)
        })
    }
}

impl Drop for Setup {
    fn drop(&mut self) {
        self.owner.hold_previews(None);
        self.owner.hold_preview_stages(None);
        self.owner.stop();
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// `preview.read` answers the job making a tier, `job.read` answers the preview it made, the next
/// read answers it ready, and the client that asked is woken.
#[test]
fn preview_cache_owner_answers_preview_read_and_job_read_and_wakes_the_client() {
    let mut setup = Setup::new("preview-owner-read");
    let file = setup.jpeg("A.JPG");
    let woken = Arc::new(AtomicUsize::new(0));
    let counter = woken.clone();
    setup.owner.watch_previews(
        setup.client,
        Arc::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        }),
    );
    let answer = setup.read(file, "grid", "visible");
    assert_eq!(answer["state"], "queued", "{answer}");
    let job = answer["job_id"].clone();
    let record = setup.settled(&job);
    assert_eq!(record["status"], "ready", "{record}");
    assert_eq!(record["kind"], "preview-extract");
    let preview = record["result"].clone();
    assert_eq!(preview["origin"], "embedded");
    assert_eq!(preview["item"], json!({"kind": "file", "file_id": file}));
    assert_eq!(
        (preview["width"].clone(), preview["height"].clone()),
        (json!(512), json!(342))
    );
    let ready = setup.read(file, "grid", "visible");
    assert_eq!(ready, json!({"state": "ready", "preview": preview}));
    let path = PathBuf::from(preview["path"].as_str().unwrap());
    assert_eq!(decoded(&path).dimensions(), (512, 342));
    wait_until("the client is woken", || woken.load(Ordering::SeqCst) > 0);

    // The loupe is read with the grid tier as the fallback meanwhile.
    let other = setup.owner.register();
    let response = setup.call(
        other,
        "preview.read",
        json!({"item": {"kind": "file", "file_id": file}, "tier": "loupe", "priority": "look-ahead"}),
    );
    let queued = response.result.unwrap();
    assert_eq!(queued["state"], "queued");
    assert_eq!(queued["fallback"], preview);
    let loupe = setup.settled(&queued["job_id"]);
    assert_eq!(loupe["result"]["tier"], "loupe");
    assert_eq!(loupe["result"]["width"], 640, "never enlarged");
}

/// The grid can draw the file's thumbnail before its embedded preview arrives: while the task is
/// held after its thumbnail stage, a read answers the same job with that stage as the fallback, and
/// the grid state is `thumbnail`; then `ready` with the embedded preview.
#[test]
fn preview_cache_owner_answers_the_thumbnail_stage_as_the_fallback() {
    let mut setup = Setup::new("preview-owner-stages");
    let file = setup.jpeg("A.JPG");
    let gate = Arc::new(Gate::new());
    gate.shut();
    setup.owner.hold_preview_stages(Some(gate.clone()));
    let first = setup.read(file, "grid", "visible");
    assert_eq!(first["state"], "queued");
    assert!(first.get("fallback").is_none(), "nothing is cached yet");
    gate.wait_reached(1, "the thumbnail stage");
    let second = setup.read(file, "grid", "visible");
    assert_eq!(second["state"], "queued");
    assert_eq!(second["job_id"], first["job_id"], "the same task");
    assert_eq!(second["fallback"]["origin"], "exif-thumbnail");
    assert_eq!(second["fallback"]["width"], 160);
    assert_eq!(second["fallback"]["height"], 107);
    assert_eq!(
        setup.owner.preview_grid_states(vec![file]),
        [PreviewState::Thumbnail]
    );
    gate.open();
    assert_eq!(setup.settled(&first["job_id"])["status"], "ready");
    let ready = setup.read(file, "grid", "visible");
    assert_eq!(ready["state"], "ready");
    assert_eq!(ready["preview"]["origin"], "embedded");
    assert_eq!(
        setup.owner.preview_grid_states(vec![file]),
        [PreviewState::Ready]
    );
}

/// With both workers held, the waiting tasks are handed out look-ahead first, then visible cells
/// newest first, then the rest oldest first; a second request joins its task, and a higher one
/// raises it.
#[test]
fn preview_cache_owner_hands_out_look_ahead_then_visible_then_background() {
    let mut setup = Setup::new("preview-owner-order");
    let files: Vec<FileId> = (0..7)
        .map(|index| setup.jpeg(&format!("F{index}.JPG")))
        .collect();
    let [a, b, c, d, e, f, g] = files[..] else {
        unreachable!()
    };
    let gate = Arc::new(Gate::new());
    gate.shut();
    setup.owner.hold_previews(Some(gate.clone()));
    let mut jobs = vec![
        setup.read(a, "grid", "visible")["job_id"].clone(),
        setup.read(b, "grid", "visible")["job_id"].clone(),
    ];
    gate.wait_reached(2, "both workers");
    jobs.push(setup.read(c, "grid", "background")["job_id"].clone());
    let first_d = setup.read(d, "grid", "visible")["job_id"].clone();
    jobs.push(setup.read(e, "grid", "look-ahead")["job_id"].clone());
    jobs.push(setup.read(f, "grid", "visible")["job_id"].clone());
    assert_eq!(
        setup.read(c, "grid", "look-ahead")["job_id"],
        jobs[2],
        "a higher request joins and raises the task"
    );
    assert_eq!(
        setup.read(d, "grid", "visible")["job_id"],
        first_d,
        "a second visible request joins and is the newest"
    );
    jobs.push(first_d);
    let other = setup.owner.register();
    let view = setup.owner.want_view(other, vec![g]).unwrap().unwrap();
    gate.open();
    for job in &jobs {
        let record = setup.settled(job);
        assert_eq!(record["status"], "ready", "{record}");
    }
    assert_eq!(setup.settled(&json!(view))["status"], "ready");
    let grid = |file| (file, PreviewTier::Grid);
    assert_eq!(
        setup.owner.previews_dispatched(),
        [
            grid(a),
            grid(b),
            grid(e),
            grid(c),
            grid(d),
            grid(f),
            grid(g)
        ]
    );
}

/// A view job reads, in the background, the grid tiers its files lack, as one job on the activity
/// board with an honest count; its cancel drops its waiting tasks while the running ones finish;
/// a newer view replaces it.
#[test]
fn preview_cache_owner_view_job_reports_progress_and_cancels() {
    let mut setup = Setup::new("preview-owner-view");
    let files: Vec<FileId> = (0..6)
        .map(|index| setup.jpeg(&format!("V{index}.JPG")))
        .collect();
    let gate = Arc::new(Gate::new());
    gate.shut();
    setup.owner.hold_previews(Some(gate.clone()));
    let job = setup
        .owner
        .want_view(setup.client, files.clone())
        .unwrap()
        .expect("the view lacks every grid tier");
    gate.wait_reached(2, "both workers");
    let board = setup.ok("activity.list", json!({}));
    let entry = board["active"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["kind"] == "preview.extract")
        .cloned()
        .expect("the view job is on the board");
    assert_eq!(entry["label"], "Reading previews");
    assert_eq!(entry["job_id"], json!(job));
    assert_eq!(
        entry["progress"],
        json!({"fraction": 0.0, "message": "0 of 6"})
    );
    assert_eq!(
        setup.ok("job.read", json!({"job_id": job}))["status"],
        "running"
    );

    let cancelled = setup.ok("job.cancel", json!({"job_id": job}));
    assert_eq!(cancelled["status"], "cancelled");
    gate.open();
    wait_until("the running tasks finish", || {
        setup.owner.preview_grid_states(files[..2].to_vec()) == [PreviewState::Ready; 2]
    });
    assert_eq!(
        setup.owner.previews_dispatched().len(),
        2,
        "the waiting four were dropped"
    );
    assert_eq!(
        setup.owner.preview_grid_states(files.clone())[2..],
        [PreviewState::Pending; 4]
    );
    let board = setup.ok("activity.list", json!({}));
    assert!(
        !board["active"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["kind"] == "preview.extract"),
        "{board}"
    );

    // A newer view replaces the client's view job; the one after it finishes with its count.
    gate.shut();
    let replaced = setup
        .owner
        .want_view(setup.client, files[2..4].to_vec())
        .unwrap()
        .unwrap();
    let job = setup
        .owner
        .want_view(setup.client, files.clone())
        .unwrap()
        .expect("four grid tiers are missing");
    assert_eq!(
        setup.ok("job.read", json!({"job_id": replaced}))["status"],
        "cancelled"
    );
    gate.open();
    let record = setup.settled(&json!(job));
    assert_eq!(record["status"], "ready", "{record}");
    assert_eq!(
        record["result"],
        json!({"files": 4, "read": 4, "failed": 0})
    );
    assert_eq!(
        record["progress"],
        json!({"fraction": 1.0, "message": "4 of 4"})
    );
    assert_eq!(
        setup.owner.want_view(setup.client, files.clone()).unwrap(),
        None,
        "nothing left to read"
    );
}

/// `job.cancel` removes a waiting task before it starts and stops a running one before it writes
/// anything.
#[test]
fn preview_cache_owner_cancels_waiting_and_running_tasks() {
    let mut setup = Setup::new("preview-owner-cancel");
    let a = setup.jpeg("A.JPG");
    let b = setup.jpeg("B.JPG");
    let c = setup.jpeg("C.JPG");
    let gate = Arc::new(Gate::new());
    gate.shut();
    setup.owner.hold_previews(Some(gate.clone()));
    let running = setup.read(a, "grid", "visible")["job_id"].clone();
    let other = setup.read(b, "grid", "visible")["job_id"].clone();
    gate.wait_reached(2, "both workers");
    let waiting = setup.read(c, "grid", "visible")["job_id"].clone();
    let removed = setup.ok("job.cancel", json!({"job_id": waiting}));
    assert_eq!(removed["status"], "cancelled");
    let stopped = setup.ok("job.cancel", json!({"job_id": running}));
    assert_eq!(stopped["status"], "cancelled");
    assert_eq!(stopped["error"]["code"], "cancelled");
    gate.open();
    assert_eq!(setup.settled(&other)["status"], "ready");
    let grid = |file| (file, PreviewTier::Grid);
    wait_until("both workers are idle again", || {
        setup.owner.previews_dispatched() == [grid(a), grid(b)]
    });
    assert_eq!(
        setup.owner.preview_grid_states(vec![a, b, c]),
        [
            PreviewState::Pending,
            PreviewState::Ready,
            PreviewState::Pending
        ],
        "the stopped task wrote nothing"
    );
    let again = setup.read(a, "grid", "visible");
    assert_eq!(again["state"], "queued");
    assert_ne!(again["job_id"], running, "a new task");
    assert_eq!(setup.settled(&again["job_id"])["status"], "ready");
}

/// List `count` files the disk does not hold, as rows only.
fn missing(index: &mut IndexDb, root: &Path, count: usize) -> Vec<FileId> {
    let tx = index.connection_mut().transaction().unwrap();
    let files = (0..count)
        .map(|n| {
            let record = FileRecord {
                path: root.join(format!("gone/{n}.JPG")),
                folder: root.join("gone"),
                name: format!("{n}.JPG"),
                volume_id: VolumeId::parse("volume-0123456789").unwrap(),
                signature: FileSignature {
                    len: 1,
                    modified_ns: n as i64,
                    identity: None,
                },
                kind: SourceTag::Jpeg,
                header: HeaderState::Pending,
                last_seen_ms: 0,
            };
            upsert_file(&tx, &record).unwrap()
        })
        .collect();
    tx.commit().unwrap();
    files
}

/// At most 20,000 tasks wait: a view that would pass it queues nothing, and a request past it is
/// refused with `resource-limit`.
#[test]
fn preview_cache_owner_refuses_past_the_queue_bound() {
    let mut setup = Setup::new("preview-owner-bound");
    let root = setup.root.clone();
    let files = missing(&mut setup.index, &root, 20_003);
    let gate = Arc::new(Gate::new());
    gate.shut();
    setup.owner.hold_previews(Some(gate.clone()));
    let error = setup
        .owner
        .want_view(setup.client, files[..20_002].to_vec())
        .unwrap_err();
    assert_eq!(error.kind, crate::ErrorKind::ResourceLimit);
    assert!(
        setup.owner.previews_dispatched().is_empty(),
        "nothing queued"
    );
    let view = setup
        .owner
        .want_view(setup.client, files[..20_000].to_vec())
        .unwrap()
        .unwrap();
    gate.wait_reached(2, "both workers");
    // Two are running, so 19,998 wait: two more fit.
    assert_eq!(
        setup.read(files[20_000], "grid", "visible")["state"],
        "queued"
    );
    assert_eq!(
        setup.read(files[20_001], "loupe", "visible")["state"],
        "queued"
    );
    assert_eq!(
        setup.failure(
            "preview.read",
            json!({"item": {"kind": "file", "file_id": files[20_002]}, "tier": "grid"})
        ),
        "resource-limit"
    );
    assert_eq!(
        setup.ok("job.cancel", json!({"job_id": view}))["status"],
        "cancelled"
    );
    gate.open();
}

/// A photograph's previews are not built yet; a file has no large tier; a file the index does not
/// hold is refused.
#[test]
fn preview_cache_owner_refuses_what_it_cannot_read() {
    let mut setup = Setup::new("preview-owner-refusals");
    let file = setup.jpeg("A.JPG");
    let read = |item: Value, tier: &str| {
        setup.failure("preview.read", json!({"item": item, "tier": tier}))
    };
    assert_eq!(
        read(
            json!({"kind": "photo", "asset_id": crate::AssetId::new()}),
            "grid"
        ),
        "unsupported-input"
    );
    assert_eq!(
        read(json!({"kind": "file", "file_id": file}), "large"),
        "validation"
    );
    assert_eq!(
        read(json!({"kind": "file", "file_id": 999_999}), "grid"),
        "validation"
    );
    assert!(setup.owner.previews_dispatched().is_empty());
}

/// A file with no usable preview fails `unsupported-input`, is remembered — read again it answers
/// the failure at once and its grid is `unavailable` — until it changes.
#[test]
fn preview_cache_owner_remembers_a_file_with_no_usable_preview() {
    let mut setup = Setup::new("preview-owner-unusable");
    let (bytes, ..) = camera_jpeg((640, 427), (160, 107), 1);
    let mut broken = bytes[..bytes.len() / 2].to_vec();
    broken.extend_from_slice(&[0xff, 0xd9]);
    let path = setup.root.join("BROKEN.JPG");
    fs::write(&path, &broken).unwrap();
    let file = add_file(
        &mut setup.index,
        &path,
        SourceTag::Jpeg,
        HeaderState::Pending,
    );
    let job = setup.read(file, "grid", "visible")["job_id"].clone();
    let record = setup.settled(&job);
    assert_eq!(record["status"], "failed");
    assert_eq!(record["error"]["code"], "unsupported-input");
    assert_eq!(
        setup.failure(
            "preview.read",
            json!({"item": {"kind": "file", "file_id": file}, "tier": "grid"})
        ),
        "unsupported-input"
    );
    assert_eq!(setup.owner.previews_dispatched().len(), 1, "not read again");
    assert_eq!(
        setup.owner.preview_grid_states(vec![file]),
        [PreviewState::Unavailable]
    );
    assert_eq!(
        setup.owner.want_view(setup.client, vec![file]).unwrap(),
        None
    );

    // Repaired and indexed again, it is read again.
    fs::write(&path, &bytes).unwrap();
    add_file(
        &mut setup.index,
        &path,
        SourceTag::Jpeg,
        HeaderState::Pending,
    );
    let job = setup.read(file, "grid", "visible")["job_id"].clone();
    assert_eq!(setup.settled(&job)["status"], "ready");
}

/// A client that disconnects loses its waker and its view job.
#[test]
fn preview_cache_owner_disconnect_ends_the_clients_view_job() {
    let mut setup = Setup::new("preview-owner-disconnect");
    let files: Vec<FileId> = (0..4)
        .map(|index| setup.jpeg(&format!("D{index}.JPG")))
        .collect();
    let gate = Arc::new(Gate::new());
    gate.shut();
    setup.owner.hold_previews(Some(gate.clone()));
    let leaving = setup.owner.register();
    let view = setup
        .owner
        .want_view(leaving, files.clone())
        .unwrap()
        .unwrap();
    gate.wait_reached(2, "both workers");
    setup.owner.disconnect(leaving);
    assert_eq!(
        setup.ok("job.read", json!({"job_id": view}))["status"],
        "cancelled"
    );
    gate.open();
    wait_until("the running tasks finish", || {
        setup.owner.previews_dispatched().len() == 2
            && setup.owner.preview_grid_states(files[..2].to_vec()) == [PreviewState::Ready; 2]
    });
    assert_eq!(
        setup.owner.preview_grid_states(files[2..].to_vec()),
        [PreviewState::Pending; 2],
        "its waiting tasks were dropped"
    );
}

/// The loupe tiers keep within the lane's budget through the owner too: the tier just read stays,
/// the one before it goes, and grid tiers are kept.
#[test]
fn preview_cache_owner_keeps_the_loupe_budget() {
    let mut setup = Setup::new("preview-owner-budget");
    let a = setup.jpeg("A.JPG");
    let b = setup.jpeg("B.JPG");
    setup.owner.preview_budget(1);
    for file in [a, b] {
        let grid = setup.read(file, "grid", "visible")["job_id"].clone();
        assert_eq!(setup.settled(&grid)["status"], "ready");
        let loupe = setup.read(file, "loupe", "look-ahead")["job_id"].clone();
        assert_eq!(setup.settled(&loupe)["status"], "ready");
    }
    assert_eq!(
        setup.read(a, "loupe", "look-ahead")["state"],
        "queued",
        "evicted"
    );
    assert_eq!(setup.read(b, "grid", "visible")["state"], "ready");
    assert_eq!(
        setup.read(a, "grid", "visible")["state"],
        "ready",
        "grids are kept"
    );
}
