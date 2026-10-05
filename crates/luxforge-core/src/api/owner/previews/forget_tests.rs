//! Forgetting photographs that left the catalog through the owner (`forget_photographs`, which
//! `asset.send-back` and `catalog.empty-removed` call once they commit): every row of
//! theirs and its file go, whatever the entry, tier or origin; their waiting and running renders
//! and camera previews are cancelled, each render tier's job naming why, and nothing of theirs is
//! written after; a view job counts them no more; other photographs keep their rows, files and
//! work; a second call does nothing; and a send-back through the API followed by the call, as
//! the library makes it.
use super::*;

/// A row of `photo`'s and its file, written as the lane writes them — a rendered tier at this
/// generation, or a camera preview — for a test to start from what an earlier session cached.
/// Answers the file.
fn cached(
    setup: &Setup,
    photo: &Photo,
    entry: &EntryId,
    tier: PreviewTier,
    origin: &str,
) -> PathBuf {
    let renderer = match origin {
        "rendered" => i64::from(RENDERER_GENERATION),
        _ => 0,
    };
    let dir = index_dir(&setup.catalog).join("previews/photos/zz");
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!(
        "{}-{entry}-{}-{origin}.jpg",
        photo.asset,
        tier.as_str()
    ));
    fs::write(&path, b"jpeg").unwrap();
    setup
        .index
        .connection()
        .execute(
            "INSERT INTO photo_previews (asset_id, entry_id, tier, renderer, path, width, height,
                 bytes, origin, last_used_ms, approximate)
             VALUES (?1, ?2, ?3, ?4, ?5, 512, 341, 4, ?6, 0, 0)",
            params![
                photo.asset.as_str(),
                entry.as_str(),
                tier.as_str(),
                renderer,
                path.to_string_lossy(),
                origin
            ],
        )
        .unwrap();
    path
}

/// The job's record, which must have ended `cancelled` because its photograph left the catalog.
fn cancelled_for_leaving(setup: &Setup, job: &Value) {
    let record = setup.ok("job.read", json!({"job_id": job}));
    assert_eq!(record["status"], "cancelled", "{record}");
    assert_eq!(record["error"]["code"], "cancelled");
    assert!(
        record["error"]["message"]
            .as_str()
            .unwrap()
            .contains("left the catalog"),
        "{record}"
    );
}

/// Three photographs leave the catalog while the lane holds work of each: `a` a running render
/// (a commit's background re-render, with a tier joined while it runs) and rows of two entries
/// and both origins, `b` a waiting render and a running camera preview, `c` a waiting render of
/// both tiers with one camera preview running and one waiting; a view job waits on `b`'s and `c`'s
/// camera previews and on `other`'s. Forgetting them deletes their rows at once and then their
/// files, ends every render tier's job `cancelled` naming why, and leaves `kept`'s row, file and
/// waiting render, and the view job's wait for `other`, as they were. Released, nothing of theirs
/// runs or is written, `kept`'s render and `other`'s camera preview complete, and the view job
/// ends counting one item. A second call changes nothing.
#[test]
fn forgetting_photographs_removes_their_rows_and_files_and_stops_their_work() {
    let (setup, photos) = Setup::new(
        "forget-owner",
        &[
            "orientation-1.jpg",
            "orientation-3.jpg",
            "orientation-6.jpg",
            "orientation-8.jpg",
            "portrait.jpg",
        ],
        true,
    );
    let [a, b, c, kept, other] = &photos[..] else {
        unreachable!()
    };
    let kept_grid = cached(&setup, kept, &kept.entry, GRID, "rendered");
    let earlier = EntryId::new();
    let a_files = [
        cached(&setup, a, &a.entry, GRID, "rendered"),
        cached(&setup, a, &a.entry, LARGE, "rendered"),
        cached(&setup, a, &earlier, GRID, "embedded"),
    ];
    let renders = Arc::new(Gate::new());
    renders.shut();
    setup.owner.hold_renders(Some(renders.clone()));
    let cameras = Arc::new(Gate::new());
    cameras.shut();
    setup.owner.hold_previews(Some(cameras.clone()));

    // `kept`'s grid tier is served from its row; its large tier waits behind `a`'s render.
    assert_eq!(setup.read(kept, GRID, "visible")["state"], "ready");
    // A commit re-renders `a`'s grid tier in the background: the running render, held.
    let (a_entry, _) = setup.edit(setup.client, a, 1, -0.8);
    renders.wait_reached(1, "the render worker");
    let a_large = setup.read(a, LARGE, "visible");
    let b_grid = setup.read(b, GRID, "visible");
    cameras.wait_reached(1, "b's camera preview");
    let c_grid = setup.read(c, GRID, "visible");
    cameras.wait_reached(2, "c's camera preview");
    let c_large = setup.read(c, LARGE, "visible");
    let kept_large = setup.read(kept, LARGE, "visible");
    let items = [b, c, other]
        .map(|photo| ViewItem::Photo(photo.row))
        .to_vec();
    let view = setup
        .owner
        .want_view_items(setup.client, items)
        .unwrap()
        .expect("three camera previews");

    setup
        .owner
        .forget_photographs(vec![a.asset.clone(), b.asset.clone(), c.asset.clone()]);
    for photo in [a, b, c] {
        assert!(
            setup.rows(photo).is_empty(),
            "every row of it is gone at once"
        );
    }
    wait_until("the forgotten photograph's files are removed", || {
        a_files.iter().all(|file| !file.exists())
    });
    for job in [&a_large, &b_grid, &c_grid, &c_large] {
        cancelled_for_leaving(&setup, &job["job_id"]);
    }
    assert!(
        !setup.ok("activity.list", json!({}))["active"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["kind"] == "preview.photo"),
        "the running render's row left the activity board"
    );
    assert_eq!(
        setup.ok("job.read", json!({"job_id": kept_large["job_id"]}))["status"],
        "queued",
        "another photograph's render waits on"
    );
    assert!(setup.has_row(kept, &kept.entry, GRID, "rendered"));
    assert!(kept_grid.exists());

    renders.open();
    cameras.open();
    assert_eq!(setup.settled(&kept_large["job_id"])["status"], "ready");
    let record = setup.settled(&json!(view));
    assert_eq!(record["status"], "ready", "{record}");
    assert_eq!(
        record["result"],
        json!({"items": 1, "read": 1, "deferred": 0, "failed": 0}),
        "only `other`'s camera preview is still the view's"
    );
    assert!(setup.has_row(other, &other.entry, GRID, "embedded"));
    assert_eq!(
        setup.owner.renders_dispatched(),
        [
            (a.asset.clone(), a_entry, vec![GRID]),
            (kept.asset.clone(), kept.entry.clone(), vec![LARGE]),
        ],
        "the waiting renders never ran"
    );
    let forgotten_rows = [b.row, c.row];
    let cameras_run: Vec<_> = setup
        .owner
        .cameras_dispatched()
        .into_iter()
        .filter(|(row, _)| forgotten_rows.contains(row))
        .collect();
    assert_eq!(
        cameras_run,
        [(b.row, GRID), (c.row, GRID)],
        "the waiting camera preview never ran"
    );
    for photo in [a, b, c] {
        assert!(
            setup.rows(photo).is_empty(),
            "the stopped work wrote nothing"
        );
    }

    // A second call finds nothing to do, and touches nothing else.
    let kept_rows = setup.rows(kept);
    setup
        .owner
        .forget_photographs(vec![a.asset.clone(), b.asset.clone(), c.asset.clone()]);
    assert_eq!(setup.rows(kept), kept_rows);
    assert!(kept_rows.iter().all(|row| row.4.exists()));
    assert_eq!(setup.read(kept, GRID, "visible")["state"], "ready");
}

/// A camera preview forgotten while it runs stays its worker's until the worker answers, so a
/// request for the same task meanwhile — a later photograph given the forgotten one's row, here
/// the same photograph asked for again — joins it rather than running beside it, and it runs again,
/// for that request, once the forgotten run has stopped.
#[test]
fn a_request_that_joins_a_forgotten_camera_preview_runs_it_again() {
    let (setup, photos) = Setup::new("forget-join", &["orientation-6.jpg"], true);
    let photo = &photos[0];
    let renders = Arc::new(Gate::new());
    renders.shut();
    setup.owner.hold_renders(Some(renders.clone()));
    let cameras = Arc::new(Gate::new());
    cameras.shut();
    setup.owner.hold_previews(Some(cameras.clone()));
    let first = setup.read(photo, GRID, "visible");
    renders.wait_reached(1, "the render worker");
    cameras.wait_reached(1, "the camera preview");
    setup.owner.forget_photographs(vec![photo.asset.clone()]);
    cancelled_for_leaving(&setup, &first["job_id"]);

    let again = setup.read(photo, GRID, "visible");
    assert_ne!(again["job_id"], first["job_id"], "a new render");
    assert_eq!(
        setup.owner.cameras_dispatched(),
        [(photo.row, GRID)],
        "joined, not run beside the forgotten one"
    );
    cameras.open();
    wait_until("the camera preview runs again and is written", || {
        setup.has_row(photo, &photo.entry, GRID, "embedded")
    });
    assert_eq!(
        setup.owner.cameras_dispatched(),
        [(photo.row, GRID), (photo.row, GRID)]
    );
    renders.open();
    assert_eq!(setup.settled(&again["job_id"])["status"], "ready");
    assert!(setup.has_row(photo, &photo.entry, GRID, "rendered"));
}

/// As the library calls it: `asset.send-back` commits, then the photographs it sent back are
/// forgotten. The photograph's rendered tiers and camera previews go, rows and files; another
/// photograph's stay, and a read of the one sent back is refused, as for any unknown photograph.
#[test]
fn a_send_back_then_forgetting_leaves_no_preview_of_the_photograph() {
    let (setup, photos) = Setup::new(
        "forget-send-back",
        &["orientation-1.jpg", "orientation-3.jpg"],
        false,
    );
    let [sent, kept] = &photos[..] else {
        unreachable!()
    };
    let files: Vec<PathBuf> = [GRID, LARGE]
        .into_iter()
        .map(|tier| path_of(&setup.ready(sent, tier)))
        .collect();
    let kept_grid = path_of(&setup.ready(kept, GRID));
    assert!(!setup.rows(sent).is_empty());

    let answer = setup.ok(
        "asset.send-back",
        json!({"targets": {"kind": "assets", "asset_ids": [sent.asset]},
               "mutation": {"request_id": "forget-send-back", "actor": "test"}}),
    );
    assert_eq!(answer["outcome"], "applied");
    setup.owner.forget_photographs(vec![sent.asset.clone()]);

    assert!(setup.rows(sent).is_empty());
    wait_until("the sent-back photograph's files are removed", || {
        files.iter().all(|file| !file.exists())
    });
    assert!(setup.has_row(kept, &kept.entry, GRID, "rendered"));
    assert!(kept_grid.exists());
    assert_eq!(
        setup.failure(
            "preview.read",
            json!({"item": {"kind": "photo", "asset_id": sent.asset}, "tier": "grid"})
        ),
        "validation"
    );
}
