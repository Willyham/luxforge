//! The preview lane through the catalog owner: `preview.read` and `job.read` over
//! [`OwnerHandle::call`], the grid's two stages, the lane's order with the workers held at a gate,
//! view jobs and their progress, `job.cancel` of queued and running work, a tier's job shared by
//! the clients that want it until the last leaves it by a cancel or a disconnect, the queue's
//! bound, the refusals, a corrupt file, the development fallback for a RAW with no usable preview,
//! and the waker.
use super::super::{ClientId, OwnerHandle};
use crate::{
    ApiRequest, ApiResponse, EditorService, SourceTag,
    catalog_types::{
        FileId, FileRecord, FileSignature, HeaderState, PreviewState, PreviewTier, VolumeId,
    },
    index::{IndexDb, index_dir, upsert_file},
    previews::preview_cache::{
        add_file, broken_jpeg, camera_jpeg, counted_development, decoded, header, synthetic_dng,
    },
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

    fn ok_for(&self, client: ClientId, method: &str, params: Value) -> Value {
        let response = self.call(client, method, params);
        assert!(response.error.is_none(), "{method}: {:?}", response.error);
        response.result.expect("a result")
    }

    fn ok(&self, method: &str, params: Value) -> Value {
        self.ok_for(self.client, method, params)
    }

    fn failure(&self, method: &str, params: Value) -> String {
        self.call(self.client, method, params)
            .error
            .unwrap_or_else(|| panic!("{method} was expected to fail"))
            .code
    }

    fn read_for(&self, client: ClientId, file: FileId, tier: &str, priority: &str) -> Value {
        self.ok_for(
            client,
            "preview.read",
            json!({"item": {"kind": "file", "file_id": file}, "tier": tier, "priority": priority}),
        )
    }

    fn read(&self, file: FileId, tier: &str, priority: &str) -> Value {
        self.read_for(self.client, file, tier, priority)
    }

    /// `job.read` of `job` by `client`: its status.
    fn status_for(&self, client: ClientId, job: &Value) -> Value {
        self.ok_for(client, "job.read", json!({"job_id": job}))["status"].clone()
    }

    /// `job.cancel` of `job` by `client`: the job's status afterwards.
    fn cancel_for(&self, client: ClientId, job: &Value) -> Value {
        self.ok_for(client, "job.cancel", json!({"job_id": job}))["status"].clone()
    }

    /// The job, as `client` reads it, once it is no longer queued or running.
    fn settled_for(&self, client: ClientId, job: &Value) -> Value {
        wait_for("the job to end", || {
            let record = self.ok_for(client, "job.read", json!({"job_id": job}));
            (!matches!(record["status"].as_str(), Some("queued" | "running"))).then_some(record)
        })
    }

    /// The job once it is no longer queued or running.
    fn settled(&self, job: &Value) -> Value {
        self.settled_for(self.client, job)
    }

    /// A new client whose wakes are counted.
    fn watching(&self) -> (ClientId, Arc<AtomicUsize>) {
        let client = self.owner.register();
        let woken = Arc::new(AtomicUsize::new(0));
        let counter = woken.clone();
        self.owner.watch_previews(
            client,
            Arc::new(move || {
                counter.fetch_add(1, Ordering::SeqCst);
            }),
        );
        (client, woken)
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
/// read answers it ready, and the client that asked is woken. A file's tiers are never
/// approximate.
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
    assert_eq!(preview["approximate"], false);
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
    let loupe = setup.settled_for(other, &queued["job_id"]);
    assert_eq!(loupe["result"]["tier"], "loupe");
    assert_eq!(loupe["result"]["width"], 640, "never enlarged");
    assert_eq!(loupe["result"]["approximate"], false);
}

/// A client is woken only for what its own requests wait on: a view job over files wakes nobody,
/// while a `preview.read` answered `queued` for one of its files wakes its client once for each of
/// the grid tier's two stages, and a `preview.region` once when it ends.
#[test]
fn preview_cache_owner_wakes_only_what_a_request_waits_on() {
    let mut setup = Setup::new("preview-owner-wakes");
    let files: Vec<FileId> = (0..4)
        .map(|index| setup.jpeg(&format!("W{index}.JPG")))
        .collect();
    let woken = Arc::new(AtomicUsize::new(0));
    let counter = woken.clone();
    setup.owner.watch_previews(
        setup.client,
        Arc::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        }),
    );
    let job = setup
        .owner
        .want_view(setup.client, files[..3].to_vec())
        .unwrap()
        .unwrap();
    assert_eq!(setup.settled(&json!(job))["status"], "ready");
    assert_eq!(woken.load(Ordering::SeqCst), 0, "a view job wakes nobody");

    let gate = Arc::new(Gate::new());
    gate.shut();
    setup.owner.hold_previews(Some(gate.clone()));
    let view = setup
        .owner
        .want_view(setup.client, files.clone())
        .unwrap()
        .expect("the fourth grid tier is lacking");
    gate.wait_reached(1, "the worker");
    let read = setup.read(files[3], "grid", "visible");
    assert_eq!(read["state"], "queued");
    gate.open();
    assert_eq!(setup.settled(&read["job_id"])["status"], "ready");
    assert_eq!(setup.settled(&json!(view))["status"], "ready");
    assert_eq!(
        woken.load(Ordering::SeqCst),
        2,
        "once for the thumbnail stage and once for the embedded preview"
    );

    let region = setup.ok(
        "preview.region",
        json!({"item": {"kind": "file", "file_id": files[0]},
               "rect": {"x": 0, "y": 0, "width": 64, "height": 64}}),
    );
    assert_eq!(setup.settled(&region["job_id"])["status"], "ready");
    assert_eq!(woken.load(Ordering::SeqCst), 3, "a region wakes its client");
}

/// A view job that wrote grid fingerprints advances the index's revision once when it ends, with
/// one index event naming it, so a view grouped before the fingerprints existed goes stale; a view
/// with nothing left to write starts no job and records nothing.
#[test]
fn preview_cache_owner_a_view_that_wrote_fingerprints_advances_the_index_revision() {
    let mut setup = Setup::new("preview-owner-revision");
    let files: Vec<FileId> = (0..3)
        .map(|index| setup.jpeg(&format!("R{index}.JPG")))
        .collect();
    let revision =
        |setup: &Setup| crate::index::database::revision(setup.index.connection()).unwrap();
    let before = revision(&setup);
    let start = setup.ok("events.since", json!({"after": 0}))["current_sequence"]
        .as_u64()
        .unwrap();
    let job = setup
        .owner
        .want_view(setup.client, files.clone())
        .unwrap()
        .unwrap();
    assert_eq!(setup.settled(&json!(job))["status"], "ready");
    let events = setup.ok("events.since", json!({"after": start}))["events"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(events.len(), 1, "one event for the view: {events:?}");
    assert_eq!(events[0]["index_revision"], before + 1);
    assert_eq!(events[0]["method"], "preview-extract");
    assert_eq!(
        events[0]["job_id"],
        json!(job),
        "the event names the view job"
    );
    assert_eq!(revision(&setup), before + 1, "once, not once a tier");

    let start = setup.ok("events.since", json!({"after": 0}))["current_sequence"]
        .as_u64()
        .unwrap();
    assert_eq!(
        setup.owner.want_view(setup.client, files).unwrap(),
        None,
        "every grid tier is there"
    );
    let events = setup.ok("events.since", json!({"after": start}))["events"]
        .as_array()
        .unwrap()
        .clone();
    assert!(events.is_empty(), "{events:?}");
    assert_eq!(revision(&setup), before + 1);
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
        json!({"items": 4, "read": 4, "deferred": 0, "failed": 0})
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

/// Two clients wait on one tier's job, which belongs to the clients that want it: one's
/// `job.cancel` releases its own interest only. The other's wait is intact — the same job, still
/// running, then `ready`, and it is woken for both of the grid tier's stages — while the client
/// that left reads the same outcome and is woken for nothing more. A client that never asked
/// neither reads nor cancels the job.
#[test]
fn preview_cache_owner_a_cancel_leaves_only_the_callers_interest() {
    let mut setup = Setup::new("preview-owner-interest");
    let file = setup.jpeg("A.JPG");
    let (leaving, left_woken) = setup.watching();
    let (staying, stayed_woken) = setup.watching();
    let gate = Arc::new(Gate::new());
    gate.shut();
    setup.owner.hold_previews(Some(gate.clone()));
    let job = setup.read_for(leaving, file, "grid", "visible")["job_id"].clone();
    gate.wait_reached(1, "the worker");
    let joined = setup.read_for(staying, file, "grid", "visible");
    assert_eq!(joined["state"], "queued");
    assert_eq!(joined["job_id"], job, "one job for the task");

    assert_eq!(
        setup.cancel_for(leaving, &job),
        "running",
        "the other client still wants it"
    );
    assert_eq!(setup.status_for(staying, &job), "running");
    assert_eq!(
        setup.status_for(leaving, &job),
        "running",
        "the client that left still reads it"
    );
    let stranger = setup.owner.register();
    for method in ["job.read", "job.cancel"] {
        let refused = setup.call(stranger, method, json!({"job_id": job}));
        assert_eq!(refused.error.expect(method).code, "validation", "{method}");
    }

    gate.open();
    let record = setup.settled_for(staying, &job);
    assert_eq!(record["status"], "ready", "{record}");
    assert_eq!(record["result"]["origin"], "embedded");
    assert_eq!(
        setup.settled_for(leaving, &job),
        record,
        "both read the one outcome"
    );
    assert_eq!(
        stayed_woken.load(Ordering::SeqCst),
        2,
        "once for the thumbnail stage and once for the embedded preview"
    );
    assert_eq!(
        left_woken.load(Ordering::SeqCst),
        0,
        "the client that left is woken for nothing"
    );
    assert_eq!(
        setup.owner.previews_dispatched(),
        [(file, PreviewTier::Grid)]
    );
}

/// A task stops only when nothing wants it. Once both clients waiting on a running tier and on a
/// queued one have cancelled, each job ends `cancelled` for both, the queued task never runs and
/// the running one writes nothing; a task a view still wants runs on for the view after its last
/// reader leaves, its job ending `cancelled`.
#[test]
fn preview_cache_owner_the_last_cancel_stops_the_task_unless_a_view_wants_it() {
    let mut setup = Setup::new("preview-owner-last-cancel");
    let [running, viewed, waiting] = ["A.JPG", "B.JPG", "C.JPG"].map(|name| setup.jpeg(name));
    let one = setup.client;
    let two = setup.owner.register();
    let gate = Arc::new(Gate::new());
    gate.shut();
    setup.owner.hold_previews(Some(gate.clone()));
    let first = setup.read_for(one, running, "grid", "visible")["job_id"].clone();
    let view = setup.owner.want_view(two, vec![viewed]).unwrap().unwrap();
    gate.wait_reached(2, "both workers");
    assert_eq!(
        setup.read_for(two, running, "grid", "visible")["job_id"],
        first
    );
    let queued = setup.read_for(one, waiting, "grid", "visible")["job_id"].clone();
    assert_eq!(
        setup.read_for(two, waiting, "grid", "visible")["job_id"],
        queued
    );
    let read_of_viewed = setup.read_for(one, viewed, "grid", "visible")["job_id"].clone();

    assert_eq!(setup.cancel_for(one, &queued), "queued");
    assert_eq!(setup.cancel_for(two, &queued), "cancelled");
    assert_eq!(setup.cancel_for(two, &first), "running");
    assert_eq!(setup.cancel_for(one, &first), "cancelled");
    for client in [one, two] {
        assert_eq!(setup.status_for(client, &queued), "cancelled");
        assert_eq!(setup.status_for(client, &first), "cancelled");
    }
    assert_eq!(
        setup.cancel_for(one, &read_of_viewed),
        "cancelled",
        "its last reader left"
    );

    gate.open();
    let record = setup.settled(&json!(view));
    assert_eq!(
        record["status"], "ready",
        "the view's task ran on: {record}"
    );
    assert_eq!(
        record["result"],
        json!({"items": 1, "read": 1, "deferred": 0, "failed": 0})
    );
    let again = setup.read(waiting, "grid", "visible");
    assert_eq!(again["state"], "queued", "the cancelled task never ran");
    assert_ne!(again["job_id"], queued, "a new task");
    assert_eq!(setup.settled(&again["job_id"])["status"], "ready");
    let grid = |file| (file, PreviewTier::Grid);
    assert_eq!(
        setup.owner.previews_dispatched(),
        [grid(running), grid(viewed), grid(waiting)]
    );
    assert_eq!(
        setup.owner.preview_grid_states(vec![running, viewed]),
        [PreviewState::Pending, PreviewState::Ready],
        "the stopped task wrote nothing"
    );
}

/// A disconnect releases the client's interest in every read it waited on, as a cancel does: a
/// task another client still waits on runs on for it, while the running and queued tasks only the
/// gone client wanted stop, the one writing nothing and the other never running.
#[test]
fn preview_cache_owner_a_disconnect_releases_the_clients_reads() {
    let mut setup = Setup::new("preview-owner-disconnect-reads");
    let [shared, alone, waiting] = ["A.JPG", "B.JPG", "C.JPG"].map(|name| setup.jpeg(name));
    let leaving = setup.owner.register();
    let gate = Arc::new(Gate::new());
    gate.shut();
    setup.owner.hold_previews(Some(gate.clone()));
    let job = setup.read(shared, "grid", "visible")["job_id"].clone();
    setup.read_for(leaving, alone, "grid", "visible");
    gate.wait_reached(2, "both workers");
    assert_eq!(
        setup.read_for(leaving, shared, "grid", "visible")["job_id"],
        job
    );
    setup.read_for(leaving, waiting, "grid", "visible");
    setup.owner.disconnect(leaving);
    assert_eq!(
        setup.status_for(setup.client, &job),
        "running",
        "the other client's wait is intact"
    );

    gate.open();
    assert_eq!(setup.settled(&job)["status"], "ready");
    let again = setup.read(waiting, "grid", "visible");
    assert_eq!(
        again["state"], "queued",
        "the gone client's queued task never ran"
    );
    assert_eq!(setup.settled(&again["job_id"])["status"], "ready");
    let grid = |file| (file, PreviewTier::Grid);
    assert_eq!(
        setup.owner.previews_dispatched(),
        [grid(shared), grid(alone), grid(waiting)]
    );
    assert_eq!(
        setup.owner.preview_grid_states(vec![alone]),
        [PreviewState::Pending],
        "the gone client's running task wrote nothing"
    );
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

/// A photograph the catalog does not hold, a file's large tier and a file the index does not hold
/// are refused.
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
        "validation"
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

/// A corrupt file fails with its own kind, `invalid-input`, and is remembered — read again it
/// answers the failure at once and its grid is `unavailable` — until it changes.
#[test]
fn preview_cache_owner_remembers_a_corrupt_file() {
    let mut setup = Setup::new("preview-owner-unusable");
    let (bytes, ..) = camera_jpeg((640, 427), (160, 107), 1);
    let path = setup.root.join("BROKEN.JPG");
    fs::write(&path, broken_jpeg()).unwrap();
    let file = add_file(
        &mut setup.index,
        &path,
        SourceTag::Jpeg,
        HeaderState::Pending,
    );
    let job = setup.read(file, "grid", "visible")["job_id"].clone();
    let record = setup.settled(&job);
    assert_eq!(record["status"], "failed");
    assert_eq!(record["error"]["code"], "invalid-input");
    assert_eq!(
        setup.failure(
            "preview.read",
            json!({"item": {"kind": "file", "file_id": file}, "tier": "grid"})
        ),
        "invalid-input"
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

/// A RAW that carries no usable preview (a synthetic DNG with none, as the Canon EOS R5 Mark II's
/// and R8's H.265-only files are to the lane) is not developed for a view in the background: the
/// view job counts it done and deferred, not failed, its grid stays `pending`, a background read
/// answers `not-ready` at once and a newer view does not read it again. A visible request develops
/// it once, labelled `developed`; the loupe's look-ahead develops its own tier.
#[test]
fn preview_cache_owner_develops_what_is_on_screen_and_defers_the_rest() {
    let mut setup = Setup::new("preview-owner-develop");
    let path = setup.root.join("H265.DNG");
    fs::write(&path, synthetic_dng("DJI", "FC3411", 64, 48)).unwrap();
    let file = add_file(
        &mut setup.index,
        &path,
        SourceTag::Raw,
        HeaderState::Pending,
    );
    let developments = Arc::new(AtomicUsize::new(0));
    setup
        .owner
        .develop_previews_with(Some(counted_development(developments.clone())));

    let view = setup
        .owner
        .want_view(setup.client, vec![file])
        .unwrap()
        .expect("the grid tier is missing");
    let record = setup.settled(&json!(view));
    assert_eq!(record["status"], "ready", "{record}");
    assert_eq!(
        record["result"],
        json!({"items": 1, "read": 0, "deferred": 1, "failed": 0})
    );
    assert_eq!(
        record["progress"],
        json!({"fraction": 1.0, "message": "1 of 1"})
    );
    assert_eq!(developments.load(Ordering::SeqCst), 0, "nothing developed");
    assert_eq!(
        setup.owner.preview_grid_states(vec![file]),
        [PreviewState::Pending]
    );
    assert_eq!(
        setup.failure(
            "preview.read",
            json!({"item": {"kind": "file", "file_id": file}, "tier": "grid", "priority": "background"})
        ),
        "not-ready"
    );
    assert_eq!(
        setup.owner.want_view(setup.client, vec![file]).unwrap(),
        None,
        "a deferred file is not read again in the background"
    );
    assert_eq!(setup.owner.previews_dispatched().len(), 1);

    let job = setup.read(file, "grid", "visible")["job_id"].clone();
    let record = setup.settled(&job);
    assert_eq!(record["status"], "ready", "{record}");
    assert_eq!(record["result"]["origin"], "developed");
    assert_eq!(developments.load(Ordering::SeqCst), 1);
    let preview = setup.read(file, "grid", "visible");
    assert_eq!(preview["state"], "ready");
    assert_eq!(preview["preview"]["origin"], "developed");
    let path = PathBuf::from(preview["preview"]["path"].as_str().unwrap());
    assert_eq!(decoded(&path).dimensions(), (96, 64));
    assert_eq!(
        setup.owner.preview_grid_states(vec![file]),
        [PreviewState::Ready]
    );
    assert_eq!(
        developments.load(Ordering::SeqCst),
        1,
        "read from the cache"
    );

    let loupe = setup.read(file, "loupe", "look-ahead")["job_id"].clone();
    let record = setup.settled(&loupe);
    assert_eq!(record["result"]["origin"], "developed", "{record}");
    assert_eq!(developments.load(Ordering::SeqCst), 2);
}

/// A background task that was deferred while a visible request joined it runs again and
/// develops; the request's job answers the developed tier.
#[test]
fn preview_cache_owner_develops_a_deferred_task_a_visible_request_joined() {
    let mut setup = Setup::new("preview-owner-develop-joined");
    let path = setup.root.join("H265.DNG");
    fs::write(&path, synthetic_dng("DJI", "FC3411", 64, 48)).unwrap();
    let file = add_file(
        &mut setup.index,
        &path,
        SourceTag::Raw,
        HeaderState::Pending,
    );
    let developments = Arc::new(AtomicUsize::new(0));
    setup
        .owner
        .develop_previews_with(Some(counted_development(developments.clone())));
    let gate = Arc::new(Gate::new());
    gate.shut();
    setup.owner.hold_previews(Some(gate.clone()));
    let view = setup
        .owner
        .want_view(setup.client, vec![file])
        .unwrap()
        .unwrap();
    gate.wait_reached(1, "the background task");
    let job = setup.read(file, "grid", "visible")["job_id"].clone();
    gate.open();
    let record = setup.settled(&job);
    assert_eq!(record["status"], "ready", "{record}");
    assert_eq!(record["result"]["origin"], "developed");
    assert_eq!(developments.load(Ordering::SeqCst), 1);
    assert_eq!(setup.settled(&json!(view))["status"], "ready");
    let grid = (file, PreviewTier::Grid);
    assert_eq!(setup.owner.previews_dispatched(), [grid, grid]);
}

/// A camera Luxforge cannot develop, through the production development, fails
/// `unsupported-input` naming why and is remembered: read again it answers at once and its grid is
/// `unavailable`.
#[test]
fn preview_cache_owner_remembers_a_raw_it_cannot_develop() {
    let mut setup = Setup::new("preview-owner-undevelopable");
    let path = setup.root.join("SENSOR.DNG");
    fs::write(&path, synthetic_dng("Example", "Sensor", 64, 48)).unwrap();
    let file = add_file(
        &mut setup.index,
        &path,
        SourceTag::Raw,
        HeaderState::Pending,
    );
    let job = setup.read(file, "grid", "visible")["job_id"].clone();
    let record = setup.settled(&job);
    assert_eq!(record["status"], "failed", "{record}");
    assert_eq!(record["error"]["code"], "unsupported-input");
    let detail = record["error"]["message"].as_str().unwrap_or_default();
    assert!(
        detail.contains("SENSOR.DNG has no usable preview")
            && detail.contains("cannot be developed"),
        "{record}"
    );
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
}

/// RAW files that carry no usable preview — the Canon EOS R5 Mark II's and R8's, whose previews
/// are H.265 only — through the production development, read in place and never written: each
/// visible grid read answers a tier labelled `developed`, and the loupe tier and a 100% region
/// come from the same kept development; or the grid fails with its own kind naming why (a mode
/// outside the RAW catalog is `unsupported-input`). Run in release with `LUXFORGE_NO_PREVIEW_RAWS`
/// naming the files, separated by `:`:
///
/// ```sh
/// LUXFORGE_NO_PREVIEW_RAWS=/path/r5m2.CR3:/path/r8.CR3 cargo test --release -p luxforge-core \
///   --lib preview_cache_owner_develops_supplied -- --ignored --nocapture
/// ```
#[test]
#[ignore = "needs LUXFORGE_NO_PREVIEW_RAWS: RAW files with no usable preview, outside Git; run in release"]
fn preview_cache_owner_develops_supplied_raws_with_no_usable_preview() {
    use sha2::{Digest, Sha256};
    let files = std::env::var("LUXFORGE_NO_PREVIEW_RAWS")
        .expect("LUXFORGE_NO_PREVIEW_RAWS names the files");
    let mut setup = Setup::new("preview-owner-no-preview-raws");
    for path in files.split(':').map(PathBuf::from) {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let before = Sha256::digest(fs::read(&path).unwrap());
        let file = add_file(
            &mut setup.index,
            &path,
            SourceTag::Raw,
            HeaderState::Pending,
        );
        let answer = setup.read(file, "grid", "visible");
        let record = setup.settled(&answer["job_id"]);
        if record["status"] == "ready" {
            let preview = &record["result"];
            assert_eq!(preview["origin"], "developed", "{name}: {record}");
            let (width, height) =
                decoded(Path::new(preview["path"].as_str().unwrap())).dimensions();
            assert_eq!(
                json!([width, height]),
                json!([preview["width"], preview["height"]])
            );
            assert_eq!(width.max(height), 512, "{name}");
            let loupe = setup.read(file, "loupe", "look-ahead");
            let loupe = &setup.settled(&loupe["job_id"])["result"];
            assert_eq!(loupe["origin"], "developed", "{name}: {loupe}");
            let region = setup.ok(
                "preview.region",
                json!({
                    "item": {"kind": "file", "file_id": file},
                    "rect": {"x": 0, "y": 0, "width": 512, "height": 512},
                    "frame": {"width": 2, "height": 2},
                }),
            );
            let region = setup.settled(&region["job_id"]);
            assert_eq!(region["result"]["origin"], "developed", "{name}: {region}");
            println!(
                "{name}: grid developed, {width} x {height}; loupe {} x {}; a 100% region of the {} x {} frame",
                loupe["width"],
                loupe["height"],
                region["result"]["frame"]["width"],
                region["result"]["frame"]["height"]
            );
        } else {
            assert_eq!(
                record["error"]["code"], "unsupported-input",
                "{name}: {record}"
            );
            println!("{name}: {}", record["error"]["message"]);
        }
        assert_eq!(
            Sha256::digest(fs::read(&path).unwrap()),
            before,
            "{name} is unchanged"
        );
    }
    crate::previews::region::release_development();
}
