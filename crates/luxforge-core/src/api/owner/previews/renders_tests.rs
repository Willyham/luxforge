//! Developed photographs' previews through the catalog owner: `preview.read` of a photograph over
//! [`OwnerHandle::call`] queuing a render and then answering it ready, both tiers from one render,
//! a commit making the next read render again with the old tier meanwhile and collecting it after,
//! another client's commit re-rendering a cached grid tier in the background, undo returning to an
//! earlier entry's key, a deleted index directory rendering again, the large tier sharing the loupe
//! tiers' budget, the discard of other renderer generations, the camera preview until the first
//! render, views over photographs making camera previews only, `job.cancel` of a waiting and a
//! running render, a tier's job shared by two clients until the last leaves it, a commit's
//! re-render stopped for everyone by any client's cancel and run on when a client that joined it
//! leaves, the render queue's bound, a backlog of renders never delaying an open photograph's
//! Develop preview and, with the supplied RAW files, an edited Nikon Z 6 photograph.
use crate::{
    ApiRequest, ApiResponse, AssetId, EditorService, EntryId, PreviewQueue, ProxyBounds, SourceTag,
    api::owner::{ClientId, OwnerHandle, PreviewRequest},
    catalog_types::{AssetRowId, FileId, PreviewState, PreviewTier, ViewItem},
    editor::mutation,
    index::{IndexDb, index_dir},
    previews::{
        preview_cache::{add_file, camera_jpeg, decoded, header},
        rendered::RENDERER_GENERATION,
    },
};
use luxforge_testbase::{Gate, paths, wait_for, wait_until};
use rusqlite::params;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    thread::JoinHandle,
};

const GRID: PreviewTier = PreviewTier::Grid;
const LARGE: PreviewTier = PreviewTier::Large;

/// One developed photograph of the setup: its identity, its row in the catalog and the entry its
/// setup left current.
#[derive(Clone, Debug)]
struct Photo {
    asset: AssetId,
    row: AssetRowId,
    entry: EntryId,
}

/// An owner over a new catalog holding developed photographs, the index beside it for the test to
/// read rows from, and a client.
struct Setup {
    root: PathBuf,
    catalog: PathBuf,
    owner: OwnerHandle,
    join: Option<JoinHandle<()>>,
    index: IndexDb,
    client: ClientId,
}

impl Setup {
    /// The setup with `photos` — each a file of `fixtures/s0`, copied in under its own name and
    /// developed into the catalog, with a Basic edit when `edited` — before the owner starts.
    fn new(name: &str, photos: &[&str], edited: bool) -> (Self, Vec<Photo>) {
        let sources: Vec<PathBuf> = photos
            .iter()
            .map(|photo| paths::fixture(&format!("s0/{photo}")))
            .collect();
        Self::of(name, &sources, |service, asset, _| {
            if edited {
                service
                    .apply_action(
                        asset,
                        mutation(0, "basic"),
                        "set-basic",
                        json!({"exposure": 0.4, "contrast": 25}),
                    )
                    .unwrap();
            }
        })
    }

    /// The setup with each of `sources` copied in under its own name and developed into the
    /// catalog, then edited by `edit` (given its position), before the owner starts.
    fn of(
        name: &str,
        sources: &[PathBuf],
        edit: impl Fn(&mut EditorService, &AssetId, usize),
    ) -> (Self, Vec<Photo>) {
        let root = paths::temp_dir(name);
        let catalog = root.join("catalog.sqlite");
        let mut service = EditorService::open(&catalog).unwrap();
        let photos = sources
            .iter()
            .enumerate()
            .map(|(index, source)| {
                let file_name = source.file_name().unwrap().to_string_lossy();
                let path = root.join(format!("{index}-{file_name}"));
                fs::copy(source, &path).unwrap();
                let asset = service.import(&path).unwrap().asset.id;
                edit(&mut service, &asset, index);
                let row: i64 = service
                    .connection
                    .query_row(
                        "SELECT row_id FROM assets WHERE id = ?1",
                        [asset.as_str()],
                        |row| row.get(0),
                    )
                    .unwrap();
                Photo {
                    entry: service.current_entry_id(&asset).unwrap(),
                    row: AssetRowId(row),
                    asset,
                }
            })
            .collect();
        let catalog_id = service.catalog_id().to_owned();
        drop(service);
        let (index, _) = IndexDb::open(&index_dir(&catalog), &catalog_id).unwrap();
        let (owner, join) = OwnerHandle::start(&catalog).unwrap();
        let client = owner.register();
        (
            Self {
                root,
                catalog,
                owner,
                join: Some(join),
                index,
                client,
            },
            photos,
        )
    }

    fn stop(&mut self) {
        self.owner.hold_renders(None);
        self.owner.hold_previews(None);
        self.owner.stop();
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }

    /// Stop the owner, delete the index directory beside the catalog, and start a new owner.
    fn restart_without_index(&mut self) {
        self.stop();
        let dir = index_dir(&self.catalog);
        fs::remove_dir_all(&dir).unwrap();
        let catalog_id = EditorService::open(&self.catalog)
            .unwrap()
            .catalog_id()
            .to_owned();
        let (owner, join) = OwnerHandle::start(&self.catalog).unwrap();
        self.owner = owner;
        self.join = Some(join);
        self.client = self.owner.register();
        self.index = IndexDb::open(&dir, &catalog_id).unwrap().0;
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

    fn read_for(
        &self,
        client: ClientId,
        photo: &Photo,
        tier: PreviewTier,
        priority: &str,
    ) -> Value {
        self.ok_for(
            client,
            "preview.read",
            json!({"item": {"kind": "photo", "asset_id": photo.asset}, "tier": tier.as_str(),
                   "priority": priority}),
        )
    }

    fn read(&self, photo: &Photo, tier: PreviewTier, priority: &str) -> Value {
        self.read_for(self.client, photo, tier, priority)
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

    /// `job.read` of `job` by `client`: its status.
    fn status_for(&self, client: ClientId, job: &Value) -> Value {
        self.ok_for(client, "job.read", json!({"job_id": job}))["status"].clone()
    }

    /// `job.cancel` of `job` by `client`: the job's status afterwards.
    fn cancel_for(&self, client: ClientId, job: &Value) -> Value {
        self.ok_for(client, "job.cancel", json!({"job_id": job}))["status"].clone()
    }

    /// The job the "Rendering previews" row on the activity board names.
    fn rendering(&self) -> Value {
        self.ok("activity.list", json!({}))["active"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["kind"] == "preview.photo")
            .map(|entry| entry["job_id"].clone())
            .expect("a render on the activity board")
    }

    /// Read `tier` of `photo` until it is ready, settling the job a queued answer names; answers
    /// the preview.
    fn ready(&self, photo: &Photo, tier: PreviewTier) -> Value {
        let answer = self.read(photo, tier, "visible");
        if answer["state"] == "ready" {
            return answer["preview"].clone();
        }
        let record = self.settled(&answer["job_id"]);
        assert_eq!(record["status"], "ready", "{record}");
        assert_eq!(record["kind"], "preview-render");
        let again = self.read(photo, tier, "visible");
        assert_eq!(again["state"], "ready", "{again}");
        assert_eq!(again["preview"], record["result"]);
        again["preview"].clone()
    }

    /// `client` commits a Basic edit to `photo` at `revision` through the API: the new current
    /// entry and revision.
    fn edit(
        &self,
        client: ClientId,
        photo: &Photo,
        revision: u64,
        exposure: f64,
    ) -> (EntryId, u64) {
        mutated(self.ok_for(
            client,
            "edit.set-basic",
            json!({"asset_id": photo.asset, "mutation": envelope(revision), "exposure": exposure}),
        ))
    }

    fn undo(&self, photo: &Photo, revision: u64) -> (EntryId, u64) {
        mutated(self.ok(
            "history.undo",
            json!({"asset_id": photo.asset, "mutation": envelope(revision)}),
        ))
    }

    /// `photo`'s rows in the index: entry, tier, renderer, origin and path.
    fn rows(&self, photo: &Photo) -> Vec<(String, String, i64, String, PathBuf)> {
        let mut statement = self
            .index
            .connection()
            .prepare(
                "SELECT entry_id, tier, renderer, origin, path FROM photo_previews
                 WHERE asset_id = ?1 ORDER BY entry_id, tier",
            )
            .unwrap();
        statement
            .query_map([photo.asset.as_str()], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    PathBuf::from(row.get::<_, String>(4)?),
                ))
            })
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    fn has_row(&self, photo: &Photo, entry: &EntryId, tier: PreviewTier, origin: &str) -> bool {
        self.rows(photo)
            .iter()
            .any(|row| row.0 == entry.as_str() && row.1 == tier.as_str() && row.3 == origin)
    }
}

impl Drop for Setup {
    fn drop(&mut self) {
        self.stop();
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn envelope(revision: u64) -> Value {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    json!({
        "expected_revision": revision,
        "request_id": format!("rendered-{}", NEXT.fetch_add(1, Ordering::Relaxed)),
        "actor": "test",
    })
}

fn mutated(answer: Value) -> (EntryId, u64) {
    (
        EntryId::parse(answer["current_entry_id"].as_str().unwrap()).unwrap(),
        answer["revision"].as_u64().unwrap(),
    )
}

fn key(photo: &Photo, entry: &EntryId, tier: PreviewTier) -> String {
    format!(
        "photo:{}:{entry}:{}:r{RENDERER_GENERATION}",
        photo.asset,
        tier.as_str()
    )
}

fn path_of(preview: &Value) -> PathBuf {
    PathBuf::from(preview["path"].as_str().unwrap())
}

fn sha256(path: &Path) -> String {
    format!("{:x}", Sha256::digest(fs::read(path).unwrap()))
}

/// A `width` × `height` JPEG of gradients and fine texture in a scratch directory of its own:
/// larger than the grid tier, so its grid tier is rendered through the proxy.
fn generated_jpeg(label: &str, width: u32, height: u32) -> PathBuf {
    let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
    for y in 0..height {
        for x in 0..width {
            rgba.extend_from_slice(&[
                (x * 255 / width) as u8,
                (y * 255 / height) as u8,
                ((x * 7 + y * 13) % 256) as u8,
                255,
            ]);
        }
    }
    let mut jpeg = Vec::new();
    let settings = luxforge_jpeg::Settings {
        quality: 92,
        chroma: (1, 1),
        segments: &[],
        icc: None,
        pixels_per_inch: None,
    };
    luxforge_jpeg::encode(&mut jpeg, width, height, &rgba, &settings, &mut |_| {
        Ok::<(), crate::Error>(())
    })
    .unwrap();
    let path = paths::temp_dir(label).join(format!("{label}.jpg"));
    fs::write(&path, jpeg).unwrap();
    path
}

/// Every preview says whether it approximates its entry, and a tier read back from the cache says
/// what its render said. A Basic edit's tiers are exact; a Clarity edit's grid tier is rendered
/// through the proxy, whose spatial neighbourhoods scale with the tier, and is approximate, while
/// its large tier, whose stage already fits, is the exact render and is not. The camera preview
/// shown until the first render is never approximate.
#[test]
fn a_rendered_tier_says_whether_it_is_approximate() {
    let sources = [
        generated_jpeg("rendered-owner-approximate-basic", 1200, 800),
        generated_jpeg("rendered-owner-approximate-clarity", 1000, 800),
    ];
    let (setup, photos) = Setup::of(
        "rendered-owner-approximate",
        &sources,
        |service, asset, at| {
            let (action, fields) = match at {
                0 => ("set-basic", json!({"exposure": 0.4, "contrast": 25})),
                _ => ("set-presence", json!({"clarity": 40})),
            };
            service
                .apply_action(asset, mutation(0, "edit"), action, fields)
                .unwrap();
        },
    );
    let [basic, clarity] = &photos[..] else {
        unreachable!()
    };
    let gate = Arc::new(Gate::new());
    gate.shut();
    setup.owner.hold_renders(Some(gate.clone()));
    let queued = setup.read(clarity, GRID, "visible");
    wait_until("the camera preview is written", || {
        setup.has_row(clarity, &clarity.entry, GRID, "embedded")
    });
    let fallback = setup.read(clarity, GRID, "visible")["fallback"].clone();
    assert_eq!(fallback["origin"], "embedded");
    assert_eq!(fallback["approximate"], false, "a camera preview is exact");
    gate.open();
    assert_eq!(setup.settled(&queued["job_id"])["status"], "ready");

    let approximate = |photo: &Photo, tier: PreviewTier| {
        let preview = setup.ready(photo, tier);
        assert_eq!(preview["origin"], "rendered");
        let stored: bool = setup
            .index
            .connection()
            .query_row(
                "SELECT approximate FROM photo_previews WHERE asset_id = ?1 AND entry_id = ?2
                   AND tier = ?3",
                params![photo.asset.as_str(), photo.entry.as_str(), tier.as_str()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            preview["approximate"],
            json!(stored),
            "the row keeps the label"
        );
        stored
    };
    assert!(
        !approximate(basic, GRID),
        "a Basic edit is exact at any size"
    );
    assert!(!approximate(basic, LARGE));
    assert!(
        approximate(clarity, GRID),
        "Clarity is approximated at 512 px"
    );
    assert!(
        !approximate(clarity, LARGE),
        "the stage fits: the exact render"
    );
}

/// A photograph's grid read queues a `preview-render` job naming the photograph, which answers the
/// rendered tier — origin `rendered`, keyed by asset, entry, tier and generation, its size its
/// file's — and the next read answers it ready. A tier the photograph's stage already fits is the
/// photograph's own size; a loupe tier or an unknown photograph is refused.
#[test]
fn a_photographs_read_queues_a_render_and_then_answers_it_ready() {
    let (setup, photos) = Setup::new("rendered-owner-read", &["orientation-6.jpg"], true);
    let photo = &photos[0];
    let answer = setup.read(photo, GRID, "visible");
    assert_eq!(answer["state"], "queued", "{answer}");
    let record = setup.settled(&answer["job_id"]);
    assert_eq!(record["status"], "ready", "{record}");
    assert_eq!(record["kind"], "preview-render");
    assert_eq!(record["asset_id"], json!(photo.asset));
    let preview = &record["result"];
    assert_eq!(preview["origin"], "rendered");
    assert_eq!(preview["tier"], "grid");
    assert_eq!(preview["key"], key(photo, &photo.entry, GRID));
    assert_eq!(
        preview["item"],
        json!({"kind": "photo", "asset_id": photo.asset, "entry_id": photo.entry})
    );
    assert_eq!(
        (preview["width"].clone(), preview["height"].clone()),
        (json!(320), json!(480)),
        "upright, and never enlarged"
    );
    let path = path_of(preview);
    assert!(path.starts_with(index_dir(&setup.catalog).join("previews/photos")));
    assert_eq!(decoded(&path).dimensions(), (320, 480));
    assert_eq!(preview["bytes"], json!(fs::metadata(&path).unwrap().len()));
    let ready = setup.read(photo, GRID, "background");
    assert_eq!(ready, json!({"state": "ready", "preview": preview}));

    let read = |item: Value, tier: &str| {
        setup.failure("preview.read", json!({"item": item, "tier": tier}))
    };
    assert_eq!(
        read(json!({"kind": "photo", "asset_id": photo.asset}), "loupe"),
        "validation"
    );
    assert_eq!(
        read(json!({"kind": "photo", "asset_id": AssetId::new()}), "grid"),
        "validation"
    );
    assert_eq!(
        read(
            json!({"kind": "photo", "asset_id": photo.asset, "entry_id": EntryId::new()}),
            "grid"
        ),
        "validation"
    );
}

/// Both tiers asked for while a render waits come from that one render, each with its own job;
/// a tier asked for while the render runs without it is rendered next.
#[test]
fn both_tiers_come_from_one_render() {
    let (setup, photos) = Setup::new(
        "rendered-owner-both",
        &["orientation-1.jpg", "orientation-3.jpg"],
        true,
    );
    let [held, photo] = &photos[..] else {
        unreachable!()
    };
    let gate = Arc::new(Gate::new());
    gate.shut();
    setup.owner.hold_renders(Some(gate.clone()));
    let first = setup.read(held, GRID, "visible");
    gate.wait_reached(1, "the render worker");
    let grid = setup.read(photo, GRID, "visible");
    let large = setup.read(photo, LARGE, "look-ahead");
    assert_ne!(grid["job_id"], large["job_id"], "a job for each tier");
    assert_eq!(
        setup.read(photo, GRID, "visible")["job_id"],
        grid["job_id"],
        "a second request joins"
    );
    // The held render makes the grid tier only; its large tier is asked for while it runs.
    let held_large = setup.read(held, LARGE, "visible");
    gate.open();
    for job in [&first, &grid, &large, &held_large] {
        let record = setup.settled(&job["job_id"]);
        assert_eq!(record["status"], "ready", "{record}");
    }
    assert_eq!(
        setup.owner.renders_dispatched(),
        [
            (held.asset.clone(), held.entry.clone(), vec![GRID]),
            (photo.asset.clone(), photo.entry.clone(), vec![GRID, LARGE]),
            (held.asset.clone(), held.entry.clone(), vec![LARGE]),
        ]
    );
    let large = setup.ready(photo, LARGE);
    assert_eq!(large["origin"], "rendered");
    assert_eq!(large["key"], key(photo, &photo.entry, LARGE));
}

/// A commit is a new entry: the next read renders it, with the old tier as the fallback, labelled
/// by its key, and once the new tier is written the old entry's rows and files are gone.
#[test]
fn a_commit_renders_again_and_collects_the_old_tier() {
    let (setup, photos) = Setup::new("rendered-owner-commit", &["orientation-1.jpg"], true);
    let photo = &photos[0];
    let old_grid = setup.ready(photo, GRID);
    let old_large = setup.ready(photo, LARGE);
    let old_pixels = sha256(&path_of(&old_grid));
    let gate = Arc::new(Gate::new());
    gate.shut();
    setup.owner.hold_renders(Some(gate.clone()));
    let (entry, _) = setup.edit(setup.client, photo, 1, -0.8);
    assert_ne!(entry, photo.entry);
    let answer = setup.read(photo, GRID, "visible");
    assert_eq!(answer["state"], "queued", "{answer}");
    assert_eq!(answer["fallback"], old_grid, "the old tier meanwhile");
    gate.open();
    let record = setup.settled(&answer["job_id"]);
    assert_eq!(record["status"], "ready", "{record}");
    let new_grid = &record["result"];
    assert_eq!(new_grid["key"], key(photo, &entry, GRID));
    assert_ne!(new_grid["path"], old_grid["path"]);
    assert_ne!(
        sha256(&path_of(new_grid)),
        old_pixels,
        "the new entry's pixels"
    );
    wait_until("the old entry's rows are collected", || {
        !setup
            .rows(photo)
            .iter()
            .any(|row| row.0 == photo.entry.as_str())
    });
    assert!(!path_of(&old_grid).exists() && !path_of(&old_large).exists());
    assert!(path_of(new_grid).exists());
}

/// Once a photograph's preview has been read, another client's commit re-renders its cached grid
/// tier in the background with nobody reading it; before any read, a commit renders nothing.
#[test]
fn another_clients_commit_rerenders_a_cached_grid_tier() {
    let (setup, photos) = Setup::new(
        "rendered-owner-follow",
        &["orientation-1.jpg", "orientation-8.jpg"],
        true,
    );
    let [photo, other] = &photos[..] else {
        unreachable!()
    };
    let agent = setup.owner.register();
    setup.edit(agent, other, 1, 0.2);
    assert!(
        setup.owner.renders_dispatched().is_empty(),
        "nothing follows commits before a photograph's preview is read"
    );

    let grid = setup.ready(photo, GRID);
    let (entry, revision) = setup.edit(agent, photo, 1, -0.6);
    wait_until(
        "the grid tier is rendered again and the old one collected",
        || setup.has_row(photo, &entry, GRID, "rendered") && !path_of(&grid).exists(),
    );
    let dispatched = setup.owner.renders_dispatched();
    assert_eq!(
        dispatched.last(),
        Some(&(photo.asset.clone(), entry.clone(), vec![GRID])),
        "a background render of the grid tier alone"
    );
    let ready = setup.read(photo, GRID, "visible");
    assert_eq!(ready["state"], "ready", "{ready}");
    assert_eq!(ready["preview"]["key"], key(photo, &entry, GRID));

    // A photograph with no rendered grid tier is left for its next read.
    setup.edit(agent, other, 2, 0.4);
    // A commit's re-render that has not started is superseded by the next commit's.
    let gate = Arc::new(Gate::new());
    gate.shut();
    setup.owner.hold_renders(Some(gate.clone()));
    let (second, revision) = setup.edit(agent, photo, revision, 0.1);
    gate.wait_reached(1, "the first re-render");
    let (superseded, revision) = setup.edit(agent, photo, revision, 0.2);
    let (third, _) = setup.edit(agent, photo, revision, 0.3);
    assert_ne!(superseded, third);
    gate.open();
    wait_until("the last commit's grid tier", || {
        setup.has_row(photo, &third, GRID, "rendered")
    });
    let rendered: Vec<EntryId> = setup
        .owner
        .renders_dispatched()
        .into_iter()
        .filter(|(asset, ..)| asset == &photo.asset)
        .map(|(_, entry, _)| entry)
        .collect();
    assert_eq!(rendered, [photo.entry.clone(), entry, second, third]);
    assert!(
        !setup
            .owner
            .renders_dispatched()
            .iter()
            .any(|(asset, ..)| asset == &other.asset)
    );
}

/// Undo returns to an earlier entry, whose tier is rendered again under its own key and file name.
#[test]
fn undo_returns_to_an_earlier_entrys_key() {
    let (setup, photos) = Setup::new("rendered-owner-undo", &["orientation-1.jpg"], true);
    let photo = &photos[0];
    let first = setup.ready(photo, GRID);
    let (entry, revision) = setup.edit(setup.client, photo, 1, -0.8);
    let second = setup.ready(photo, GRID);
    assert_eq!(second["key"], key(photo, &entry, GRID));
    let (back, _) = setup.undo(photo, revision);
    assert_eq!(back, photo.entry);
    let answer = setup.read(photo, GRID, "visible");
    if answer["state"] == "queued" {
        assert_eq!(answer["fallback"]["key"], second["key"]);
    }
    let again = setup.ready(photo, GRID);
    assert_eq!(again["key"], first["key"]);
    assert_eq!(again["path"], first["path"]);
    assert_eq!(sha256(&path_of(&again)), sha256(&path_of(&first)));
}

/// Deleting the index directory loses nothing but time: read again, the tier is rendered again,
/// the same key and bytes.
#[test]
fn deleting_the_index_directory_renders_again() {
    let (mut setup, photos) = Setup::new("rendered-owner-deleted", &["orientation-1.jpg"], true);
    let photo = &photos[0];
    let grid = setup.ready(photo, GRID);
    let bytes = fs::read(path_of(&grid)).unwrap();
    setup.restart_without_index();
    let answer = setup.read(photo, GRID, "visible");
    assert_eq!(answer["state"], "queued");
    assert!(answer.get("fallback").is_none(), "nothing is cached");
    let again = setup.ready(photo, GRID);
    assert_eq!(again["key"], grid["key"]);
    assert_eq!(fs::read(path_of(&again)).unwrap(), bytes);
}

/// Large tiers share the loupe tiers' byte budget, least recently used out first and never the one
/// just written; grid tiers are kept. A served large tier records its use at most once a minute.
#[test]
fn the_large_tier_shares_the_loupe_budget() {
    let (mut setup, photos) = Setup::new(
        "rendered-owner-budget",
        &["orientation-1.jpg", "orientation-3.jpg"],
        true,
    );
    let [a, b] = &photos[..] else { unreachable!() };
    let (bytes, offset, len) = camera_jpeg((640, 427), (160, 107), 1);
    let path = setup.root.join("LOUPE.JPG");
    fs::write(&path, &bytes).unwrap();
    let file = add_file(
        &mut setup.index,
        &path,
        SourceTag::Jpeg,
        header(1, Some((offset, len, (160, 107)))),
    );
    let loupe = |setup: &Setup| {
        let answer = setup.ok(
            "preview.read",
            json!({"item": {"kind": "file", "file_id": file}, "tier": "loupe"}),
        );
        if answer["state"] == "queued" {
            setup.settled(&answer["job_id"]);
        }
    };
    loupe(&setup);
    let loupe_rows = |setup: &Setup| -> i64 {
        setup
            .index
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM previews WHERE file_id = ?1 AND tier = 'loupe'",
                [file.0],
                |row| row.get(0),
            )
            .unwrap()
    };
    assert_eq!(loupe_rows(&setup), 1);
    setup.owner.preview_budget(1);
    let a_large = setup.ready(a, LARGE);
    assert_eq!(
        loupe_rows(&setup),
        0,
        "the loupe tier was least recently used"
    );
    let a_grid = setup.ready(a, GRID);
    let b_large = setup.ready(b, LARGE);
    assert!(
        !setup.has_row(a, &a.entry, LARGE, "rendered"),
        "a's large tier went"
    );
    assert!(!path_of(&a_large).exists());
    assert!(path_of(&b_large).exists(), "never the one just written");
    assert!(path_of(&a_grid).exists(), "grid tiers are kept");

    // A served large tier records its use, at most once a minute.
    let used = |setup: &Setup| -> i64 {
        setup
            .index
            .connection()
            .query_row(
                "SELECT last_used_ms FROM photo_previews WHERE asset_id = ?1 AND tier = 'large'",
                [b.asset.as_str()],
                |row| row.get(0),
            )
            .unwrap()
    };
    setup
        .index
        .connection()
        .execute(
            "UPDATE photo_previews SET last_used_ms = 0 WHERE asset_id = ?1",
            [b.asset.as_str()],
        )
        .unwrap();
    assert_eq!(setup.read(b, LARGE, "visible")["state"], "ready");
    let first = used(&setup);
    assert!(first > 0);
    assert_eq!(setup.read(b, LARGE, "visible")["state"], "ready");
    assert_eq!(used(&setup), first, "not again within the minute");
}

/// When the render worker starts, the rows of other renderer generations are discarded with their
/// files; this generation's rows and camera previews stay.
#[test]
fn another_generations_rows_are_discarded() {
    let (setup, photos) = Setup::new(
        "rendered-owner-generation",
        &["orientation-1.jpg", "orientation-3.jpg"],
        true,
    );
    let [a, b] = &photos[..] else { unreachable!() };
    let previews = index_dir(&setup.catalog).join("previews/photos/zz");
    fs::create_dir_all(&previews).unwrap();
    let rows = [
        (
            &b.asset,
            EntryId::new(),
            "grid",
            i64::from(RENDERER_GENERATION) + 1,
            "rendered",
        ),
        (
            &b.asset,
            EntryId::new(),
            "large",
            i64::from(RENDERER_GENERATION) + 7,
            "rendered",
        ),
        (&b.asset, EntryId::new(), "grid", 0, "embedded"),
        (
            &b.asset,
            b.entry.clone(),
            "grid",
            i64::from(RENDERER_GENERATION),
            "rendered",
        ),
    ];
    let files: Vec<PathBuf> = rows
        .iter()
        .enumerate()
        .map(|(n, (asset, entry, tier, renderer, origin))| {
            let path = previews.join(format!("{n}.jpg"));
            fs::write(&path, b"jpeg").unwrap();
            setup
                .index
                .connection()
                .execute(
                    "INSERT INTO photo_previews VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
                    params![
                        asset.as_str(),
                        entry.as_str(),
                        tier,
                        renderer,
                        path.to_string_lossy(),
                        512,
                        341,
                        4,
                        origin,
                        0,
                        false
                    ],
                )
                .unwrap();
            path
        })
        .collect();
    setup.ready(a, GRID);
    wait_until("the other generations are discarded", || {
        setup.rows(b).len() == 2
    });
    let kept: Vec<(i64, String)> = setup
        .rows(b)
        .into_iter()
        .map(|row| (row.2, row.3))
        .collect();
    assert!(kept.contains(&(0, "embedded".into())));
    assert!(kept.contains(&(i64::from(RENDERER_GENERATION), "rendered".into())));
    assert!(!files[0].exists() && !files[1].exists());
    assert!(files[2].exists() && files[3].exists());
}

/// Until its first render a photograph shows its camera preview: the read that queues the render
/// asks the extraction workers for it, and the next read answers it as the fallback, labelled
/// `embedded`. The render then replaces it. For a JPEG with no edits the camera preview is the
/// original itself, fitted.
#[test]
fn the_camera_preview_is_the_fallback_until_the_first_render() {
    let (setup, photos) = Setup::new("rendered-owner-camera", &["orientation-6.jpg"], false);
    let photo = &photos[0];
    let woken = Arc::new(AtomicU64::new(0));
    let counter = woken.clone();
    setup.owner.watch_previews(
        setup.client,
        Arc::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        }),
    );
    let gate = Arc::new(Gate::new());
    gate.shut();
    setup.owner.hold_renders(Some(gate.clone()));
    let first = setup.read(photo, GRID, "visible");
    assert_eq!(first["state"], "queued");
    assert!(first.get("fallback").is_none(), "nothing is cached yet");
    wait_until("the camera preview is written", || {
        setup.has_row(photo, &photo.entry, GRID, "embedded")
    });
    assert_eq!(setup.owner.cameras_dispatched(), [(photo.row, GRID)]);
    let second = setup.read(photo, GRID, "visible");
    assert_eq!(second["job_id"], first["job_id"], "the same render");
    let fallback = &second["fallback"];
    assert_eq!(fallback["origin"], "embedded");
    assert_eq!(
        fallback["key"],
        format!("photo:{}:{}:grid:embedded", photo.asset, photo.entry)
    );
    assert_eq!(
        (fallback["width"].clone(), fallback["height"].clone()),
        (json!(320), json!(480)),
        "upright"
    );
    assert_eq!(decoded(&path_of(fallback)).dimensions(), (320, 480));
    gate.open();
    let rendered = setup.ready(photo, GRID);
    assert_eq!(rendered["origin"], "rendered");
    assert_eq!(
        setup.rows(photo).len(),
        1,
        "the render replaced the camera preview's row"
    );
    assert!(!path_of(fallback).exists());
    wait_until(
        "the reader is woken for the camera preview and the render",
        || woken.load(Ordering::SeqCst) == 2,
    );
    // With a render of the tier, no camera preview is asked for again.
    setup.edit(setup.client, photo, 0, 0.5);
    setup.read(photo, GRID, "visible");
    assert_eq!(setup.owner.cameras_dispatched().len(), 1);
}

/// A view over photographs makes, in the background, only the camera previews of those with no
/// grid row, never a render; the grid states follow the renders a visible read and a commit make.
#[test]
fn views_over_photographs_make_camera_previews_only() {
    let (setup, photos) = Setup::new(
        "rendered-owner-view",
        &[
            "orientation-1.jpg",
            "orientation-3.jpg",
            "orientation-6.jpg",
        ],
        true,
    );
    let items: Vec<ViewItem> = photos
        .iter()
        .map(|photo| ViewItem::Photo(photo.row))
        .collect();
    let job = setup
        .owner
        .want_view_items(setup.client, items.clone())
        .unwrap()
        .expect("three camera previews");
    let record = setup.settled(&json!(job));
    assert_eq!(record["status"], "ready", "{record}");
    assert_eq!(
        record["result"],
        json!({"items": 3, "read": 3, "deferred": 0, "failed": 0})
    );
    assert!(setup.owner.renders_dispatched().is_empty(), "no render");
    assert_eq!(setup.owner.cameras_dispatched().len(), 3);
    assert_eq!(
        setup.owner.view_grid_states(items.clone()),
        [PreviewState::Thumbnail; 3]
    );
    assert_eq!(
        setup
            .owner
            .want_view_items(setup.client, items.clone())
            .unwrap(),
        None,
        "nothing left to read"
    );

    setup.ready(&photos[0], GRID);
    assert_eq!(
        setup.owner.view_grid_states(items.clone()),
        [
            PreviewState::Ready,
            PreviewState::Thumbnail,
            PreviewState::Thumbnail
        ]
    );
    let gate = Arc::new(Gate::new());
    gate.shut();
    setup.owner.hold_renders(Some(gate.clone()));
    let (entry, _) = setup.edit(setup.client, &photos[0], 1, -0.5);
    assert_eq!(
        setup.owner.view_grid_states(items.clone())[0],
        PreviewState::Thumbnail,
        "a stale render until its re-render"
    );
    gate.open();
    wait_until("the re-render", || {
        setup.has_row(&photos[0], &entry, GRID, "rendered")
    });
    assert_eq!(setup.owner.view_grid_states(items)[0], PreviewState::Ready);
    // A file in the same view is answered as the file lane answers it.
    assert_eq!(
        setup
            .owner
            .view_grid_states(vec![ViewItem::File(FileId(999_999))]),
        [PreviewState::Pending]
    );
}

/// `job.cancel` removes a waiting render and stops a running one before it writes anything; read
/// again, the photograph renders.
#[test]
fn job_cancel_removes_a_waiting_render_and_stops_a_running_one() {
    let (setup, photos) = Setup::new(
        "rendered-owner-cancel",
        &[
            "orientation-1.jpg",
            "orientation-3.jpg",
            "orientation-6.jpg",
        ],
        true,
    );
    let [running, waiting, next] = &photos[..] else {
        unreachable!()
    };
    let gate = Arc::new(Gate::new());
    gate.shut();
    setup.owner.hold_renders(Some(gate.clone()));
    let first = setup.read(running, GRID, "visible");
    gate.wait_reached(1, "the render worker");
    let second = setup.read(waiting, GRID, "visible");
    let removed = setup.ok("job.cancel", json!({"job_id": second["job_id"]}));
    assert_eq!(removed["status"], "cancelled");
    let stopped = setup.ok("job.cancel", json!({"job_id": first["job_id"]}));
    assert_eq!(stopped["status"], "cancelled");
    assert_eq!(stopped["error"]["code"], "cancelled");
    assert!(
        !setup.ok("activity.list", json!({}))["active"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["kind"] == "preview.photo"),
        "its row left the activity board"
    );
    gate.open();
    // Renders run one at a time, so the next one starts once the stopped one has ended.
    let after = setup.read(next, GRID, "visible");
    assert_eq!(setup.settled(&after["job_id"])["status"], "ready");
    assert_eq!(
        setup.owner.renders_dispatched(),
        [
            (running.asset.clone(), running.entry.clone(), vec![GRID]),
            (next.asset.clone(), next.entry.clone(), vec![GRID]),
        ]
    );
    assert!(
        !setup.has_row(running, &running.entry, GRID, "rendered"),
        "the stopped render wrote nothing"
    );
    let again = setup.read(running, GRID, "visible");
    assert_ne!(again["job_id"], first["job_id"], "a new render");
    assert_eq!(setup.settled(&again["job_id"])["status"], "ready");
}

/// Two clients wait on one tier's render job, which belongs to the clients that want it: one's
/// `job.cancel` releases its own interest only. The other's wait is intact — the same job, still
/// running, then `ready`, and it is woken for the camera preview and for the render — while the
/// client that left reads the same outcome and is woken for neither.
#[test]
fn a_tiers_job_is_shared_and_a_cancel_releases_only_the_callers_interest() {
    let (setup, photos) = Setup::new("rendered-owner-interest", &["orientation-6.jpg"], false);
    let photo = &photos[0];
    let watching = || {
        let client = setup.owner.register();
        let woken = Arc::new(AtomicU64::new(0));
        let counter = woken.clone();
        setup.owner.watch_previews(
            client,
            Arc::new(move || {
                counter.fetch_add(1, Ordering::SeqCst);
            }),
        );
        (client, woken)
    };
    let (leaving, left_woken) = watching();
    let (staying, stayed_woken) = watching();
    let renders = Arc::new(Gate::new());
    renders.shut();
    setup.owner.hold_renders(Some(renders.clone()));
    let cameras = Arc::new(Gate::new());
    cameras.shut();
    setup.owner.hold_previews(Some(cameras.clone()));
    let job = setup.read_for(leaving, photo, GRID, "visible")["job_id"].clone();
    renders.wait_reached(1, "the render worker");
    cameras.wait_reached(1, "the camera preview");
    assert_eq!(
        setup.read_for(staying, photo, GRID, "visible")["job_id"],
        job,
        "one job for the tier"
    );

    assert_eq!(
        setup.cancel_for(leaving, &job),
        "running",
        "the other client still wants it"
    );
    assert_eq!(setup.status_for(staying, &job), "running");
    assert_eq!(setup.status_for(leaving, &job), "running");
    cameras.open();
    wait_until("the camera preview is written", || {
        setup.has_row(photo, &photo.entry, GRID, "embedded")
    });
    renders.open();
    let record = setup.settled_for(staying, &job);
    assert_eq!(record["status"], "ready", "{record}");
    assert_eq!(record["result"]["origin"], "rendered");
    assert_eq!(
        setup.settled_for(leaving, &job),
        record,
        "both read the one outcome"
    );
    wait_until(
        "the client still waiting is woken for the camera preview and the render",
        || stayed_woken.load(Ordering::SeqCst) == 2,
    );
    assert_eq!(
        left_woken.load(Ordering::SeqCst),
        0,
        "the client that left is woken for neither"
    );
    assert_eq!(
        setup.owner.renders_dispatched(),
        [(photo.asset.clone(), photo.entry.clone(), vec![GRID])]
    );
}

/// A render stops only when no client wants a tier of it. Once both clients waiting on a running
/// render and on a waiting one have left — by `job.cancel`, or the last by disconnecting — each
/// tier's job ends `cancelled`, the waiting render never runs and the running one writes nothing.
#[test]
fn only_the_last_client_to_leave_a_tiers_job_stops_its_render() {
    let (setup, photos) = Setup::new(
        "rendered-owner-last-cancel",
        &[
            "orientation-1.jpg",
            "orientation-3.jpg",
            "orientation-6.jpg",
        ],
        true,
    );
    let [running, waiting, next] = &photos[..] else {
        unreachable!()
    };
    let other = setup.owner.register();
    let leaving = setup.owner.register();
    let gate = Arc::new(Gate::new());
    gate.shut();
    setup.owner.hold_renders(Some(gate.clone()));
    let first = setup.read_for(leaving, running, GRID, "visible")["job_id"].clone();
    gate.wait_reached(1, "the render worker");
    assert_eq!(
        setup.read_for(other, running, GRID, "visible")["job_id"],
        first
    );
    let second = setup.read(waiting, GRID, "visible")["job_id"].clone();
    assert_eq!(
        setup.read_for(other, waiting, GRID, "visible")["job_id"],
        second
    );

    assert_eq!(setup.cancel_for(setup.client, &second), "queued");
    assert_eq!(setup.cancel_for(other, &second), "cancelled");
    assert_eq!(setup.cancel_for(other, &first), "running");
    setup.owner.disconnect(leaving);
    assert_eq!(
        setup.status_for(other, &first),
        "cancelled",
        "its last client gone, the render stopped"
    );
    for client in [setup.client, other] {
        assert_eq!(setup.status_for(client, &second), "cancelled");
    }

    gate.open();
    // Renders run one at a time, so the next one starts once the stopped one has ended.
    let after = setup.read(next, GRID, "visible");
    assert_eq!(setup.settled(&after["job_id"])["status"], "ready");
    assert_eq!(
        setup.owner.renders_dispatched(),
        [
            (running.asset.clone(), running.entry.clone(), vec![GRID]),
            (next.asset.clone(), next.entry.clone(), vec![GRID]),
        ]
    );
    assert!(
        !setup.has_row(running, &running.entry, GRID, "rendered"),
        "the stopped render wrote nothing"
    );
}

/// A commit's background re-render has a job of its own that no client waits for: any client — one
/// that never asked, as the Performance section's Cancel is — reads it, and its `job.cancel` stops
/// it for everyone, answering the job, and drops the commit's want, so the render writes nothing
/// and nothing renders it again.
#[test]
fn any_clients_cancel_of_a_commits_rerender_stops_it_for_everyone() {
    let (setup, photos) = Setup::new(
        "rendered-owner-stop-rerender",
        &["orientation-1.jpg", "orientation-3.jpg"],
        true,
    );
    let [photo, other] = &photos[..] else {
        unreachable!()
    };
    setup.ready(photo, GRID);
    let gate = Arc::new(Gate::new());
    gate.shut();
    setup.owner.hold_renders(Some(gate.clone()));
    let (entry, _) = setup.edit(setup.client, photo, 1, -0.8);
    gate.wait_reached(1, "the commit's re-render");
    let job = setup.rendering();
    let stranger = setup.owner.register();
    assert_eq!(setup.status_for(stranger, &job), "running");

    let record = setup.ok_for(stranger, "job.cancel", json!({"job_id": job}));
    assert_eq!(record["kind"], "preview-render");
    assert_eq!(record["status"], "cancelled", "{record}");
    assert_eq!(record["error"]["code"], "cancelled");
    assert_eq!(setup.status_for(setup.client, &job), "cancelled");
    assert!(
        !setup.ok("activity.list", json!({}))["active"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["kind"] == "preview.photo"),
        "its row left the activity board"
    );

    gate.open();
    // Renders run one at a time, so the next one starts once the stopped one has ended.
    let after = setup.read(other, GRID, "visible");
    assert_eq!(setup.settled(&after["job_id"])["status"], "ready");
    assert!(
        !setup.has_row(photo, &entry, GRID, "rendered"),
        "the stopped re-render wrote nothing"
    );
    assert_eq!(
        setup.owner.renders_dispatched(),
        [
            (photo.asset.clone(), photo.entry.clone(), vec![GRID]),
            (photo.asset.clone(), entry, vec![GRID]),
            (other.asset.clone(), other.entry.clone(), vec![GRID]),
        ],
        "nothing renders the commit's tier again"
    );
}

/// A commit's re-render wants its tier as a view wants a file's: a client that reads the tier while
/// it renders waits on the job the requests share, and its `job.cancel` ends that job `cancelled`,
/// releasing only itself, while the render runs on for the commit to `ready` and writes the tier.
#[test]
fn a_client_that_leaves_a_commits_rerender_leaves_it_running() {
    let (setup, photos) = Setup::new(
        "rendered-owner-leave-rerender",
        &["orientation-1.jpg"],
        true,
    );
    let photo = &photos[0];
    setup.ready(photo, GRID);
    let gate = Arc::new(Gate::new());
    gate.shut();
    setup.owner.hold_renders(Some(gate.clone()));
    let (entry, _) = setup.edit(setup.client, photo, 1, -0.8);
    gate.wait_reached(1, "the commit's re-render");
    let background = setup.rendering();
    let read = setup.read(photo, GRID, "visible");
    assert_eq!(read["state"], "queued", "{read}");
    let job = read["job_id"].clone();
    assert_ne!(job, background, "the requests' own job");
    assert_eq!(setup.status_for(setup.client, &job), "running");

    assert_eq!(setup.cancel_for(setup.client, &job), "cancelled");
    assert_eq!(
        setup.status_for(setup.client, &background),
        "running",
        "the commit still wants the tier"
    );
    gate.open();
    let record = setup.settled(&background);
    assert_eq!(record["status"], "ready", "{record}");
    assert_eq!(record["result"]["key"], key(photo, &entry, GRID));
    assert!(setup.has_row(photo, &entry, GRID, "rendered"));
    assert_eq!(setup.status_for(setup.client, &job), "cancelled");
    assert_eq!(
        setup.owner.renders_dispatched(),
        [
            (photo.asset.clone(), photo.entry.clone(), vec![GRID]),
            (photo.asset.clone(), entry, vec![GRID]),
        ],
        "one render, never stopped"
    );
}

/// The render queue is bounded: past it a new render is `resource-limit`, while a request that
/// joins a waiting render still fits.
#[test]
fn the_render_queue_is_bounded() {
    let (setup, photos) = Setup::new(
        "rendered-owner-bound",
        &[
            "orientation-1.jpg",
            "orientation-3.jpg",
            "orientation-6.jpg",
        ],
        true,
    );
    setup.owner.render_capacity(1);
    let gate = Arc::new(Gate::new());
    gate.shut();
    setup.owner.hold_renders(Some(gate.clone()));
    setup.read(&photos[0], GRID, "visible");
    gate.wait_reached(1, "the render worker");
    let waiting = setup.read(&photos[1], GRID, "visible");
    assert_eq!(waiting["state"], "queued");
    assert_eq!(
        setup.failure(
            "preview.read",
            json!({"item": {"kind": "photo", "asset_id": photos[2].asset}, "tier": "grid"})
        ),
        "resource-limit"
    );
    assert_eq!(
        setup.read(&photos[1], LARGE, "visible")["state"],
        "queued",
        "a tier of a waiting render joins it"
    );
    gate.open();
    assert_eq!(setup.settled(&waiting["job_id"])["status"], "ready");
}

/// The acceptance's ordering check through the owner. With the render worker held on one render
/// and more renders waiting behind it — the open photograph's own large tier among them — the
/// photograph Develop has open asks for its preview: its exact frame arrives through the editor's preview queue while every render is still held or waiting, so the
/// preview never waited behind them. Released, the backlog completes.
#[test]
fn a_rendered_backlog_never_delays_an_open_develop_preview() {
    let (setup, photos) = Setup::new(
        "rendered-owner-backlog",
        &[
            "orientation-1.jpg",
            "orientation-3.jpg",
            "orientation-8.jpg",
        ],
        true,
    );
    // The open photograph, opened through the API as a client opens a file (a Develop, its
    // preparation and its adoption): its source is in the editor's cache.
    let open_path = setup.root.join("open.jpg");
    fs::copy(paths::fixture("s0/orientation-6.jpg"), &open_path).unwrap();
    let state = crate::api::owner::library::opening::open(&setup.owner, setup.client, &open_path);
    let open = AssetId::parse(state["asset"]["id"].as_str().unwrap()).unwrap();
    let open_photo = Photo {
        asset: open.clone(),
        row: AssetRowId(0),
        entry: EntryId::parse(state["current_entry"]["id"].as_str().unwrap()).unwrap(),
    };

    let gate = Arc::new(Gate::new());
    gate.shut();
    setup.owner.hold_renders(Some(gate.clone()));
    let mut backlog: Vec<Value> = photos
        .iter()
        .flat_map(|photo| [GRID, LARGE].map(|tier| setup.read(photo, tier, "visible")))
        .collect();
    backlog.push(setup.read(&open_photo, LARGE, "look-ahead"));
    gate.wait_reached(1, "the render worker");

    let job = setup
        .owner
        .preview_job(
            PreviewRequest::new(setup.client, open.clone())
                .proxy(ProxyBounds {
                    width: 256,
                    height: 256,
                })
                .analyse(),
        )
        .expect("the open photograph's preview job");
    let mut queue = PreviewQueue::default();
    queue.request(job);
    let exact = wait_for("Develop's exact frame", || queue.poll());
    let outcome = exact.exact().expect("the exact phase");
    assert!(outcome.result.is_ok() && outcome.report.is_some());
    assert!(
        gate.holding() && gate.waiting() == 1,
        "the render worker was held throughout"
    );
    assert_eq!(setup.owner.renders_dispatched().len(), 1, "the rest waited");
    for answer in &backlog {
        let record = setup.ok("job.read", json!({"job_id": answer["job_id"]}));
        assert!(
            matches!(record["status"].as_str(), Some("queued" | "running")),
            "{record}"
        );
    }

    gate.open();
    for answer in &backlog {
        assert_eq!(setup.settled(&answer["job_id"])["status"], "ready");
    }
}

/// An edited Nikon Z 6 photograph in a catalog the owner reopens: its camera preview first (the
/// embedded JPEG, labelled `embedded`), then both tiers from one render, each fitted to its long
/// edge, and the file unchanged. Set `LUXFORGE_RAW_OWNER_DIR` to the directory holding it and run
/// in release:
///
/// ```text
/// LUXFORGE_RAW_OWNER_DIR=/path/to/raw cargo test --release -p luxforge-core --lib \
///   preview_rendered_owner -- --ignored --nocapture
/// ```
#[test]
#[ignore = "requires the supplied RAW files; run in release"]
fn a_supplied_raw_shows_its_camera_preview_then_renders_both_tiers() {
    let owner_dir = PathBuf::from(std::env::var("LUXFORGE_RAW_OWNER_DIR").expect("RAW directory"));
    let original = owner_dir.join("nikon_z6.NEF");
    let before = sha256(&original);
    let root = paths::temp_dir("rendered-owner-raw");
    let catalog = root.join("catalog.sqlite");
    let (asset, entry) = {
        let mut service = EditorService::open(&catalog).unwrap();
        let asset = service.import(&original).unwrap().asset.id;
        service
            .apply_action(
                &asset,
                mutation(0, "basic"),
                "set-basic",
                json!({"exposure": 0.3, "contrast": 20}),
            )
            .unwrap();
        service
            .apply_action(
                &asset,
                mutation(1, "presence"),
                "set-presence",
                json!({"clarity": 30}),
            )
            .unwrap();
        let entry = service.current_entry_id(&asset).unwrap();
        (asset, entry)
    };
    let (owner, join) = OwnerHandle::start(&catalog).unwrap();
    let client = owner.register();
    let call = |method: &str, params: Value| -> Value {
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
            .unwrap();
        assert!(response.error.is_none(), "{method}: {:?}", response.error);
        response.result.unwrap()
    };
    let settled = |job: &Value| -> Value {
        wait_for("the job to end", || {
            let record = call("job.read", json!({"job_id": job}));
            (!matches!(record["status"].as_str(), Some("queued" | "running"))).then_some(record)
        })
    };
    let read = |tier: &str| {
        call(
            "preview.read",
            json!({"item": {"kind": "photo", "asset_id": asset}, "tier": tier}),
        )
    };
    let gate = Arc::new(Gate::new());
    gate.shut();
    owner.hold_renders(Some(gate.clone()));
    let started = std::time::Instant::now();
    let large = read("large");
    gate.wait_reached(1, "the render worker");
    let camera = wait_for("the camera preview", || {
        let answer = read("large");
        answer.get("fallback").cloned()
    });
    // Asked for while the large tier's render runs, the grid tier is rendered next.
    let grid = read("grid");
    println!(
        "nikon_z6.NEF: camera preview {}×{} after {:?}",
        camera["width"],
        camera["height"],
        started.elapsed()
    );
    assert_eq!(camera["origin"], "embedded");
    assert_eq!(
        camera["width"]
            .as_u64()
            .unwrap()
            .max(camera["height"].as_u64().unwrap()),
        2048
    );
    gate.open();
    for (tier, answer, side) in [("grid", &grid, 512), ("large", &large, 2048)] {
        let record = settled(&answer["job_id"]);
        assert_eq!(record["status"], "ready", "{record}");
        let preview = &record["result"];
        assert_eq!(preview["origin"], "rendered");
        assert_eq!(
            preview["key"],
            format!("photo:{asset}:{entry}:{tier}:r{RENDERER_GENERATION}")
        );
        let (width, height) = decoded(&path_of(preview)).dimensions();
        assert_eq!(width.max(height), side, "{tier}");
        println!(
            "nikon_z6.NEF: {tier} tier {width}×{height}, {} bytes",
            preview["bytes"]
        );
    }
    println!("nikon_z6.NEF: both tiers after {:?}", started.elapsed());
    assert_eq!(
        owner.renders_dispatched(),
        [
            (asset.clone(), entry.clone(), vec![LARGE]),
            (asset.clone(), entry.clone(), vec![GRID]),
        ]
    );
    owner.hold_renders(None);
    owner.stop();
    let _ = join.join();
    assert_eq!(sha256(&original), before, "nikon_z6.NEF changed");
    let _ = fs::remove_dir_all(&root);
}

#[path = "forget_tests.rs"]
mod preview_forget;
