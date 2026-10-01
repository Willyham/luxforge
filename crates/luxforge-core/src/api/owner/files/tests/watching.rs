//! The index keeping up by itself through the platform's change notifications, over real scratch
//! folders (canonical paths) and a fixed mount table: changes in an indexed folder applied by
//! signature without a refresh, what indexing skips kept out, a rescan listing again what was not
//! reported, a rescan as a job a client cancels and the stale folder it leaves, a folder no longer
//! indexed no longer watched, changes made while Luxforge was closed caught up as it opens (macOS),
//! volumes mounted and taken out, a card mounted as the lane starts, the one event every listing
//! records as it ends, and stopping with the watcher running.
use super::*;
use crate::index::lane::{VolumeEvent, WatchEvent};
use luxforge_watch::RescanReason;
use std::sync::mpsc::sync_channel;

/// How many header reads the lane has taken in.
fn reads(owner: &OwnerHandle) -> usize {
    let (reply, answer) = sync_channel(1);
    tell(owner, FilesMessage::HeaderReads(reply));
    answer.recv().unwrap()
}

/// The indexed folder at `path` as `index.folders` answers it.
fn folder(owner: &OwnerHandle, client: ClientId, path: &Path) -> Value {
    ok(owner, client, "index.folders", json!({}))["folders"]
        .as_array()
        .unwrap()
        .iter()
        .find(|folder| folder["path"] == json!(path))
        .cloned()
        .expect("the folder is indexed")
}

/// Add `path` as an indexed folder, wait for its listing, and wait until the lane watches it.
fn add_watched(owner: &OwnerHandle, client: ClientId, path: &Path, request: &str) {
    let added = ok(
        owner,
        client,
        "index.add-folder",
        json!({"path": path, "mutation": envelope(request)}),
    );
    assert_eq!(finished(owner, client, &added["job_id"])["status"], "ready");
    wait_for("the folder to be watched", || {
        (folder(owner, client, path)["watching"] == true).then_some(())
    });
}

/// Make `path` hold `bytes` in one step: written in `staging`, outside every watched folder, then
/// moved in, so the watcher never sees it half written.
fn arrive(staging: &Path, path: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(staging).unwrap();
    let written = staging.join(path.file_name().unwrap());
    std::fs::write(&written, bytes).unwrap();
    std::fs::rename(&written, path).unwrap();
}

/// The rows under `root` whose header has been read, by path, with their ids.
fn read_rows(fixture: &Fixture, root: &Path) -> BTreeMap<PathBuf, i64> {
    fixture
        .rows()
        .into_iter()
        .filter(|(path, _, state)| path.starts_with(root) && state != "pending")
        .map(|(path, id, _)| (path, id))
        .collect()
}

/// The paths of `rows`, each under `root` as `names` name them, in order.
fn paths(root: &Path, names: &[&str]) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = names.iter().map(|name| root.join(name)).collect();
    paths.sort();
    paths
}

fn events_after(owner: &OwnerHandle, client: ClientId, after: u64) -> Vec<Value> {
    ok(owner, client, "events.since", json!({"after": after}))["events"]
        .as_array()
        .unwrap()
        .clone()
}

fn sequence(owner: &OwnerHandle, client: ClientId) -> u64 {
    ok(owner, client, "events.since", json!({"after": 0}))["current_sequence"]
        .as_u64()
        .unwrap()
}

/// A file added, one replaced by other bytes, one renamed, a folder moved in with a file and a
/// folder removed in an indexed folder reach the index with no `index.refresh`: the new and the
/// changed files are read, the renamed one keeps its row unread, the removed rows go, and each
/// batch is announced as work no request made.
#[test]
fn an_indexed_folder_keeps_up_with_its_files_without_a_refresh() {
    let fixture = Fixture::new("watch-changes");
    let owner = fixture.owner();
    let client = owner.register();
    let photos = fixture.dir.join("photos");
    let staging = fixture.dir.join("staging");
    let camera = camera_jpeg();
    for name in ["a.jpg", "b.jpg", "kept.jpg", "gone/c.jpg", "gone/d.jpg"] {
        put(&photos.join(name), &camera);
    }
    add_watched(owner, client, &photos, "add");
    let ids = read_rows(&fixture, &photos);
    assert_eq!(ids.len(), 5);
    let start = sequence(owner, client);
    let before = reads(owner);

    arrive(&staging, &photos.join("new.jpg"), &camera);
    arrive(
        &staging,
        &photos.join("a.jpg"),
        &[camera.as_slice(), b"edited"].concat(),
    );
    std::fs::rename(photos.join("b.jpg"), photos.join("renamed.jpg")).unwrap();
    put(&staging.join("moved in/e.jpg"), &camera);
    std::fs::rename(staging.join("moved in"), photos.join("moved in")).unwrap();
    std::fs::remove_dir_all(photos.join("gone")).unwrap();

    let expected = paths(
        &photos,
        &[
            "a.jpg",
            "kept.jpg",
            "moved in/e.jpg",
            "new.jpg",
            "renamed.jpg",
        ],
    );
    let rows = wait_for("the index to follow the folder", || {
        let rows = read_rows(&fixture, &photos);
        (rows.keys().cloned().collect::<Vec<_>>() == expected).then_some(rows)
    });
    assert_eq!(
        rows[&photos.join("renamed.jpg")],
        ids[&photos.join("b.jpg")],
        "the renamed file keeps its row"
    );
    assert_eq!(rows[&photos.join("a.jpg")], ids[&photos.join("a.jpg")]);
    assert_eq!(
        rows[&photos.join("kept.jpg")],
        ids[&photos.join("kept.jpg")]
    );
    let connection = database::connect_at(&fixture.index_dir()).unwrap();
    let edited = database::file(
        &connection,
        crate::catalog_types::FileId(rows[&photos.join("a.jpg")]),
    )
    .unwrap()
    .unwrap();
    assert_eq!(edited.signature.len, camera.len() as u64 + 6);
    assert_eq!(
        reads(owner) - before,
        3,
        "new.jpg, a.jpg and e.jpg are read; renamed.jpg is carried unread"
    );
    let events = events_after(owner, client, start);
    assert!(!events.is_empty());
    for event in &events {
        assert_eq!(
            (&event["method"], &event["request_id"]),
            (&json!("index-watch"), &json!("")),
            "{event}"
        );
        assert!(event["index_revision"].is_u64(), "{event}");
    }
}

/// Hard links added to an indexed folder of links to a few files, reported in one change, are each
/// their own row, read once, and a link renamed keeps its row unread: nothing else moves. (The new
/// links are of other files than the renamed one: a new link of a file one of whose links is gone
/// is that link moved, as a rename is.)
#[cfg(unix)]
#[test]
fn an_indexed_folder_of_hard_links_keeps_up_with_links_added_and_renamed() {
    const LINKS: usize = 300;
    let fixture = Fixture::new("watch-links");
    let owner = fixture.owner();
    let client = owner.register();
    let photos = fixture.dir.join("photos");
    let sources: Vec<PathBuf> = (0..3)
        .map(|index| fixture.dir.join(format!("sources/source-{index}.jpg")))
        .collect();
    for source in &sources {
        put(source, &camera_jpeg());
    }
    std::fs::create_dir_all(&photos).unwrap();
    let link = |number: usize| photos.join(format!("IMG_{number:04}.jpg"));
    for number in 0..LINKS {
        std::fs::hard_link(&sources[number % 3], link(number)).unwrap();
    }
    add_watched(owner, client, &photos, "add");
    let ids = read_rows(&fixture, &photos);
    assert_eq!(ids.len(), LINKS);
    let before = reads(owner);

    let renamed = photos.join("renamed.jpg");
    std::fs::rename(link(5), &renamed).unwrap();
    // The renamed link is of the third file; the new links are of the other two.
    for number in LINKS..2 * LINKS {
        std::fs::hard_link(&sources[number % 2], link(number)).unwrap();
    }
    // Every path in one change, as the watcher may report them, whatever it reports itself.
    let mut changed: Vec<PathBuf> = (LINKS..2 * LINKS).map(link).collect();
    changed.extend([link(5), renamed.clone()]);
    tell(
        owner,
        FilesMessage::Inject(WatchEvent::Changed {
            root: 1,
            paths: changed,
            cursor: None,
        }),
    );
    let rows = wait_for("the index to follow the links", || {
        let rows = read_rows(&fixture, &photos);
        (rows.len() == 2 * LINKS && rows.contains_key(&renamed)).then_some(rows)
    });
    assert_eq!(
        rows[&renamed],
        ids[&link(5)],
        "the renamed link keeps its row"
    );
    assert!(
        ids.iter()
            .filter(|(path, _)| **path != link(5))
            .all(|(path, id)| rows.get(path) == Some(id)),
        "every other link keeps its row"
    );
    let added: std::collections::HashSet<i64> = (LINKS..2 * LINKS)
        .map(|number| rows[&link(number)])
        .collect();
    assert_eq!(added.len(), LINKS, "each new link its own row");
    assert!(ids.values().all(|id| !added.contains(id)), "none moved");
    assert_eq!(
        reads(owner) - before,
        LINKS,
        "each new link is read; the renamed one is carried unread"
    );
}

/// What indexing skips stays out as it changes: hidden files and folders, packages, other
/// applications' caches, links and unsupported files; a skipped folder renamed to one that is
/// listed is listed, and one renamed to a package is dropped.
#[test]
fn what_indexing_skips_stays_out_as_it_changes() {
    let fixture = Fixture::new("watch-skips");
    let owner = fixture.owner();
    let client = owner.register();
    let photos = fixture.dir.join("photos");
    let staging = fixture.dir.join("staging");
    let camera = camera_jpeg();
    put(&photos.join("a.jpg"), &camera);
    add_watched(owner, client, &photos, "add");

    for name in [
        ".hidden/x.jpg",
        ".dot.jpg",
        "Library.photoslibrary/y.jpg",
        "Catalog Previews.lrdata/z.jpg",
        "sub/.thumbnails/w.jpg",
        "notes.txt",
    ] {
        put(&photos.join(name), &camera);
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(photos.join("a.jpg"), photos.join("link.jpg")).unwrap();
    // The last change: once it is in, every earlier one was applied.
    arrive(&staging, &photos.join("last.jpg"), &camera);
    wait_for("the last change", || {
        read_rows(&fixture, &photos)
            .contains_key(&photos.join("last.jpg"))
            .then_some(())
    });
    assert_eq!(
        read_rows(&fixture, &photos)
            .keys()
            .cloned()
            .collect::<Vec<_>>(),
        paths(&photos, &["a.jpg", "last.jpg"])
    );

    std::fs::rename(photos.join(".hidden"), photos.join("shown")).unwrap();
    wait_for("the folder shown to be listed", || {
        read_rows(&fixture, &photos)
            .contains_key(&photos.join("shown/x.jpg"))
            .then_some(())
    });
    std::fs::rename(photos.join("shown"), photos.join("Also.photoslibrary")).unwrap();
    wait_for("the folder made a package to be dropped", || {
        (read_rows(&fixture, &photos)
            .keys()
            .cloned()
            .collect::<Vec<_>>()
            == paths(&photos, &["a.jpg", "last.jpg"]))
        .then_some(())
    });
}

/// A rescan lists again what the watcher could not report: a subtree alone, then the whole root.
#[test]
fn a_rescan_lists_again_what_was_not_reported() {
    let fixture = Fixture::new("watch-rescan");
    let owner = fixture.owner();
    let client = owner.register();
    let photos = fixture.dir.join("photos");
    for name in ["a.jpg", "sub/b.jpg"] {
        put(&photos.join(name), &camera_jpeg());
    }
    add_watched(owner, client, &photos, "add");
    // A last change, so every earlier one the watcher delivers late has been applied first.
    arrive(
        &fixture.dir.join("staging"),
        &photos.join("sentinel.jpg"),
        &camera_jpeg(),
    );
    wait_for("the last change", || {
        read_rows(&fixture, &photos)
            .contains_key(&photos.join("sentinel.jpg"))
            .then_some(())
    });
    // Changes nothing reported: the rows are gone from the index behind the lane's back.
    let connection = database::connect_at(&fixture.index_dir()).unwrap();
    for name in ["a.jpg", "sub/b.jpg"] {
        connection
            .execute(
                "DELETE FROM files WHERE path = ?1",
                [photos.join(name).to_string_lossy()],
            )
            .unwrap();
    }
    // The lane watches this one root, the first it kept.
    tell(
        owner,
        FilesMessage::Inject(WatchEvent::Rescan {
            root: 1,
            subtree: photos.join("sub"),
            reason: RescanReason::Dropped,
        }),
    );
    wait_for("the subtree to be listed again", || {
        read_rows(&fixture, &photos)
            .contains_key(&photos.join("sub/b.jpg"))
            .then_some(())
    });
    assert!(
        !read_rows(&fixture, &photos).contains_key(&photos.join("a.jpg")),
        "only the subtree"
    );
    tell(
        owner,
        FilesMessage::Inject(WatchEvent::Rescan {
            root: 1,
            subtree: photos.clone(),
            reason: RescanReason::Overflow,
        }),
    );
    wait_for("the root to be listed again", || {
        (read_rows(&fixture, &photos)
            .keys()
            .cloned()
            .collect::<Vec<_>>()
            == paths(&photos, &["a.jpg", "sentinel.jpg", "sub/b.jpg"]))
        .then_some(())
    });
}

/// When the root at `path` was last listed in full.
fn listed_ms(fixture: &Fixture, path: &Path) -> Option<i64> {
    let connection = database::connect_at(&fixture.index_dir()).unwrap();
    database::root(&connection, path)
        .unwrap()
        .and_then(|root| root.listed_ms)
}

/// Hold the lane's next listing at its first folder with `gate`, after a rescan of the whole of the
/// first root it keeps, `photos`, and answer that listing's board entry once it is held.
fn held_rescan(owner: &OwnerHandle, client: ClientId, gate: &Gate, photos: &Path) -> Value {
    gate.shut();
    let reached = gate.reached();
    tell(
        owner,
        FilesMessage::Inject(WatchEvent::Rescan {
            root: 1,
            subtree: photos.to_path_buf(),
            reason: RescanReason::Overflow,
        }),
    );
    gate.wait_reached(reached + 1, "the rescan's first folder");
    let board = ok(owner, client, "activity.list", json!({}));
    let running: Vec<Value> = board["active"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|entry| entry["kind"] == "index.refresh")
        .cloned()
        .collect();
    assert_eq!(running.len(), 1, "{board}");
    running[0].clone()
}

/// The watcher's rescan is an `index-refresh` job no request started: on the board with its job,
/// read with `job.read` while it runs and stopped by `job.cancel`, its end one event of work no
/// request made naming it. Cancelled, it leaves its folder stale — on its row and in
/// `index.folders` — until a listing of it completes: before its next change is applied, and as the
/// catalog next opens.
#[test]
fn a_rescan_is_a_job_and_a_cancelled_one_leaves_its_folder_stale_until_it_is_listed() {
    let mut fixture = Fixture::new("watch-rescan-job");
    let photos = fixture.dir.join("photos");
    let staging = fixture.dir.join("staging");
    for name in ["a/1.jpg", "b/2.jpg", "c/3.jpg"] {
        put(&photos.join(name), &camera_jpeg());
    }
    let gate = Arc::new(Gate::new());
    {
        let owner = fixture.owner();
        let client = owner.register();
        // Every listing is held at each folder while the gate is shut, from the lane's start.
        tell(owner, FilesMessage::Hold(gate.clone()));
        add_watched(owner, client, &photos, "add");
        // A last change, so every earlier one the watcher delivers late has been applied first.
        arrive(&staging, &photos.join("sentinel.jpg"), &camera_jpeg());
        wait_for("the last change", || {
            read_rows(&fixture, &photos)
                .contains_key(&photos.join("sentinel.jpg"))
                .then_some(())
        });
        let start = sequence(owner, client);

        let entry = held_rescan(owner, client, &gate, &photos);
        assert_eq!(
            (&entry["label"], &entry["detail"]),
            (&json!("Indexing"), &json!(photos)),
            "{entry}"
        );
        let job_id = entry["job_id"].clone();
        let job = ok(owner, client, "job.read", json!({"job_id": job_id}));
        assert_eq!(
            (&job["status"], &job["kind"]),
            (&json!("running"), &json!("index-refresh")),
            "{job}"
        );
        // Against the extent the folder's last listing found, before the sentinel arrived.
        assert_eq!(
            job["progress"]["message"], "0 of about 3 files",
            "the listing's own count: {job}"
        );
        ok(owner, client, "job.cancel", json!({"job_id": job_id}));
        let job = finished(owner, client, &job_id);
        gate.open();
        assert_eq!(job["status"], "cancelled", "{job}");
        assert_eq!(folder(owner, client, &photos)["stale"], true);
        let connection = database::connect_at(&fixture.index_dir()).unwrap();
        assert!(
            database::root_stale(&connection, &photos).unwrap(),
            "on its row"
        );
        let events = events_after(owner, client, start);
        let last = events.last().expect("the job's end");
        assert_eq!(
            (&last["method"], &last["request_id"], &last["job_id"]),
            (&json!("index-watch"), &json!(""), &job_id),
            "{events:?}"
        );
        assert!(last["index_revision"].is_u64(), "{last}");

        // Its next change is applied after a listing of the whole folder, which makes it current.
        let before = listed_ms(&fixture, &photos);
        arrive(&staging, &photos.join("new.jpg"), &camera_jpeg());
        wait_for("the stale folder to be listed on its next change", || {
            let current = folder(owner, client, &photos).get("stale").is_none();
            (current && read_rows(&fixture, &photos).contains_key(&photos.join("new.jpg")))
                .then_some(())
        });
        assert!(listed_ms(&fixture, &photos) > before, "listed in full");
        assert!(!database::root_stale(&connection, &photos).unwrap());

        // Cancelled again, it is still stale as the catalog closes.
        let entry = held_rescan(owner, client, &gate, &photos);
        ok(
            owner,
            client,
            "job.cancel",
            json!({"job_id": entry["job_id"]}),
        );
        assert_eq!(
            finished(owner, client, &entry["job_id"])["status"],
            "cancelled"
        );
        gate.open();
        assert_eq!(folder(owner, client, &photos)["stale"], true);
    }
    let before = listed_ms(&fixture, &photos);
    fixture.stop();
    fixture.start();
    let owner = fixture.owner();
    let client = owner.register();
    wait_for("the stale folder to be listed as the catalog opens", || {
        (folder(owner, client, &photos).get("stale").is_none()
            && listed_ms(&fixture, &photos) > before)
            .then_some(())
    });
    assert_eq!(folder(owner, client, &photos)["watching"], true);
}

/// A card already mounted as a catalog with an indexed folder opens is listed as one mounted while
/// Luxforge runs would be: an `index.refresh` job no request started, once the first survey after
/// the lane started has found it, with no client asking about the disk; opened again with more on
/// the card, it is reconciled again.
#[test]
fn a_card_mounted_as_the_catalog_opens_is_listed() {
    let mut fixture = Fixture::new("watch-card-at-open");
    let (table, card, drive) = mounts(&fixture);
    {
        let owner = fixture.owner();
        let client = owner.register();
        add_watched(owner, client, &drive.join("2026/trip"), "add");
    }
    fixture.stop();
    // The mount table from the catalog's opening: the lane starts as it opens, with the watch.
    super::super::OPENING_MOUNTS
        .lock()
        .unwrap()
        .push((fixture.catalog.clone(), MountSource::Fixed(table)));
    let dcim = card.join("DCIM");
    let listing = |fixture: &Fixture, files: u32| {
        wait_for("the card to be listed as the catalog opens", || {
            let connection = database::connect_at(&fixture.index_dir()).unwrap();
            database::root(&connection, &dcim)
                .unwrap()
                .filter(|root| root.file_count == Some(files))
                .map(|_| ())
        })
    };
    for (files, request) in [(1, "first"), (2, "second")] {
        if files == 2 {
            put(&card.join("DCIM/100NZ6_1/DSC_0002.jpg"), &camera_jpeg());
        }
        fixture.start();
        listing(&fixture, files);
        let owner = fixture.owner();
        let client = owner.register();
        let events = events_after(owner, client, 0);
        let ended = events
            .iter()
            .rev()
            .find(|event| event["method"] == "index-watch" && event["job_id"].is_string())
            .unwrap_or_else(|| {
                panic!("{request}: the listing is work no request made: {events:?}")
            });
        assert_eq!(ended["request_id"], "", "{ended}");
        let job = finished(owner, client, &ended["job_id"]);
        assert_eq!(
            (&job["kind"], &job["status"], &job["result"]["roots"]),
            (&json!("index-refresh"), &json!("ready"), &json!([dcim])),
            "{request}: {job}"
        );
        fixture.stop();
    }
    super::super::OPENING_MOUNTS
        .lock()
        .unwrap()
        .retain(|(catalog, _)| *catalog != fixture.catalog);
}

/// Removing an indexed folder stops its watch: what changes in it later is not indexed, while
/// another indexed folder is still followed.
#[test]
fn a_folder_no_longer_indexed_is_no_longer_watched() {
    let fixture = Fixture::new("watch-removed");
    let owner = fixture.owner();
    let client = owner.register();
    let photos = fixture.dir.join("photos");
    let other = fixture.dir.join("other");
    let staging = fixture.dir.join("staging");
    put(&photos.join("a.jpg"), &camera_jpeg());
    put(&other.join("b.jpg"), &camera_jpeg());
    add_watched(owner, client, &photos, "add-photos");
    add_watched(owner, client, &other, "add-other");
    let start = sequence(owner, client);
    ok(
        owner,
        client,
        "index.remove-folder",
        json!({"path": photos, "mutation": envelope("remove")}),
    );
    wait_for("the removed folder's rows to go", || {
        read_rows(&fixture, &photos).is_empty().then_some(())
    });
    // Forgetting its rows is announced under the request that removed it.
    wait_for("the forgotten rows' event", || {
        events_after(owner, client, start)
            .iter()
            .any(|event| {
                event["method"] == "index.remove-folder" && event["index_revision"].is_u64()
            })
            .then_some(())
    });
    arrive(&staging, &photos.join("late.jpg"), &camera_jpeg());
    arrive(&staging, &other.join("sentinel.jpg"), &camera_jpeg());
    wait_for("the other folder to be followed", || {
        read_rows(&fixture, &other)
            .contains_key(&other.join("sentinel.jpg"))
            .then_some(())
    });
    assert!(read_rows(&fixture, &photos).is_empty(), "not followed");
    let folders = ok(owner, client, "index.folders", json!({}))["folders"].clone();
    assert_eq!(folders.as_array().unwrap().len(), 1);
    assert_eq!(folders[0]["watching"], true);
}

/// What changed while Luxforge was closed is replayed from each root's kept cursor as it opens,
/// with no listing and no client asking; a cursor of another history cannot replay, and the folder
/// is listed again instead.
#[cfg(target_os = "macos")]
#[test]
fn changes_made_while_luxforge_was_closed_are_caught_up_as_it_opens() {
    let mut fixture = Fixture::new("watch-replay");
    let photos = fixture.dir.join("photos");
    let staging = fixture.dir.join("staging");
    let camera = camera_jpeg();
    for name in ["a.jpg", "b.jpg", "c.jpg"] {
        put(&photos.join(name), &camera);
    }
    {
        let owner = fixture.owner();
        let client = owner.register();
        add_watched(owner, client, &photos, "add");
    }
    let root = |fixture: &Fixture| {
        let connection = database::connect_at(&fixture.index_dir()).unwrap();
        (
            database::root(&connection, &photos).unwrap().unwrap(),
            database::root_cursor(&connection, &photos).unwrap(),
        )
    };
    wait_for("the root's cursor to be kept", || root(&fixture).1);
    let listed = root(&fixture).0.listed_ms;
    fixture.stop();

    arrive(&staging, &photos.join("new.jpg"), &camera);
    arrive(
        &staging,
        &photos.join("b.jpg"),
        &[camera.as_slice(), b"edited"].concat(),
    );
    std::fs::remove_file(photos.join("c.jpg")).unwrap();
    fixture.start();
    let owner = fixture.owner();
    let client = owner.register();
    wait_for("what changed while closed to be caught up", || {
        let caught_up = read_rows(&fixture, &photos)
            .keys()
            .cloned()
            .collect::<Vec<_>>()
            == paths(&photos, &["a.jpg", "b.jpg", "new.jpg"])
            && reads(owner) == 2;
        caught_up.then_some(())
    });
    assert_eq!(root(&fixture).0.listed_ms, listed, "replayed, not listed");
    assert_eq!(folder(owner, client, &photos)["watching"], true);

    // A cursor of another history: listed again as the catalog opens.
    fixture.stop();
    database::connect_at(&fixture.index_dir())
        .unwrap()
        .execute(
            "UPDATE roots SET cursor_volume = ?2 WHERE path = ?1",
            rusqlite::params![photos.to_string_lossy(), [9_u8; 16].to_vec()],
        )
        .unwrap();
    arrive(&staging, &photos.join("late.jpg"), &camera);
    fixture.start();
    wait_for("the folder to be listed again", || {
        read_rows(&fixture, &photos)
            .contains_key(&photos.join("late.jpg"))
            .then_some(())
    });
    assert!(root(&fixture).0.listed_ms > listed, "listed again");
}

/// A volume the watcher reports mounted is surveyed, and listed when it is a card; one taken out
/// is gone from `card.list` at once, its roots offline, and an indexed folder on it no longer
/// watched.
#[test]
fn a_card_mounted_is_listed_and_a_volume_taken_out_goes_offline() {
    let fixture = Fixture::new("watch-volumes");
    let owner = fixture.owner();
    let client = owner.register();
    let (table, _, drive) = mounts(&fixture);
    let trip = drive.join("2026/trip");
    add_watched(owner, client, &trip, "add");

    let lumix = fixture.dir.join("LUMIX");
    put(&lumix.join("DCIM/100_PANA/P1000001.JPG"), &camera_jpeg());
    let mount = crate::index::volumes::tests::mount_at(&lumix, "LUMIX", 4, true);
    table.lock().unwrap().push(mount.clone());
    tell(
        owner,
        FilesMessage::Inject(WatchEvent::Volume(VolumeEvent::Mounted { mount })),
    );
    let card = |owner: &OwnerHandle| {
        ok(owner, client, "card.list", json!({}))["cards"]
            .as_array()
            .unwrap()
            .iter()
            .find(|card| card["volume"]["label"] == "LUMIX")
            .cloned()
    };
    wait_for("the card to be listed", || {
        card(owner).filter(|card| card["files"] == 1)
    });

    table
        .lock()
        .unwrap()
        .retain(|mount| mount.mount_point != lumix);
    tell(
        owner,
        FilesMessage::Inject(WatchEvent::Volume(VolumeEvent::Unmounted {
            mount_point: lumix.clone(),
        })),
    );
    assert!(card(owner).is_none(), "gone from card.list at once");
    wait_for("the card's root to go offline", || {
        let connection = database::connect_at(&fixture.index_dir()).unwrap();
        database::root(&connection, &lumix.join("DCIM"))
            .unwrap()
            .filter(|root| root.offline)
    });

    table
        .lock()
        .unwrap()
        .retain(|mount| mount.mount_point != drive);
    std::fs::rename(&drive, fixture.dir.join("unplugged")).unwrap();
    tell(
        owner,
        FilesMessage::Inject(WatchEvent::Volume(VolumeEvent::Unmounted {
            mount_point: drive.clone(),
        })),
    );
    let state = wait_for("the drive's folder to be unwatched", || {
        Some(folder(owner, client, &trip)).filter(|state| state["watching"] == false)
    });
    assert_eq!(state["offline"], true, "{state}");
    assert!(state["unwatched"].is_string(), "{state}");
}

/// Every `index.refresh` job records one event as it ends, naming its request and the index
/// revision it left: one that changed nothing, and one that failed.
#[test]
fn every_listing_records_one_event_as_it_ends() {
    let fixture = Fixture::new("watch-ended");
    let owner = fixture.owner();
    let client = owner.register();
    tell(
        owner,
        FilesMessage::Limits(WalkLimits {
            files: 2,
            ..WalkLimits::default()
        }),
    );
    let photos = fixture.dir.join("photos");
    put(&photos.join("a.jpg"), &camera_jpeg());
    refresh(owner, client, json!({"kind": "folder", "path": photos}));

    let start = sequence(owner, client);
    let report = refresh(owner, client, json!({"kind": "folder", "path": photos}));
    assert_eq!(report["headers_read"], 0, "nothing changed");
    // The batch that records the root's listing time, then the job's end, naming the revision it
    // left.
    let events = events_after(owner, client, start);
    assert_eq!(events.len(), 2, "{events:?}");
    for event in &events {
        assert_eq!(
            (
                &event["method"],
                &event["request_id"],
                &event["index_revision"]
            ),
            (
                &json!("index.refresh"),
                &json!("index.refresh"),
                &json!(fixture.revision())
            )
        );
    }

    let big = fixture.dir.join("big");
    for index in 0..3 {
        put(&big.join(format!("{index}/a.jpg")), b"x");
    }
    let start = sequence(owner, client);
    let started = ok(
        owner,
        client,
        "index.refresh",
        json!({"source": {"kind": "folder", "path": big}}),
    );
    assert_eq!(
        finished(owner, client, &started["job_id"])["status"],
        "failed"
    );
    let events = events_after(owner, client, start);
    let last = events.last().expect("the job's end");
    assert_eq!(last["request_id"], "index.refresh");
    assert_eq!(last["index_revision"], fixture.revision());
    assert_eq!(last["job_id"], started["job_id"], "the end names its job");
}

/// Stopping the owner with the watcher following a folder stops the watcher and joins every lane
/// thread.
#[test]
fn stopping_with_the_watcher_running_joins_cleanly() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let mut fixture = Fixture::new("watch-stop");
    let photos = fixture.dir.join("photos");
    put(&photos.join("a.jpg"), &camera_jpeg());
    {
        let owner = fixture.owner();
        let client = owner.register();
        add_watched(owner, client, &photos, "add");
    }
    let (owner, join) = fixture.owner.take().unwrap();
    let stopped = Arc::new(AtomicBool::new(false));
    let stopper = {
        let stopped = stopped.clone();
        std::thread::spawn(move || {
            owner.stop();
            join.join().unwrap();
            stopped.store(true, Ordering::SeqCst);
        })
    };
    luxforge_testbase::wait_until("the owner and its lanes to stop", || {
        stopped.load(Ordering::SeqCst)
    });
    stopper.join().unwrap();
}
