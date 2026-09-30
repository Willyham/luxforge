//! Availability and Locate through the catalog owner, as clients call them: the source-recovery
//! spec's Locate acceptance (a moved original located and its edits exported, a byte-identical
//! copy, what is not the original, a file another photograph names, a file changed around its
//! verification, a cancel and a failed commit, undo), `source.check` over a same-volume move found
//! again by identity, and, on macOS, availability and refusals across a disk image's detach and
//! re-attach. Every original is a copied fixture in a scratch directory.
use super::super::{Owner, OwnerMessage, catalog::CatalogMessage};
use super::{Hold, LibraryMessage};
use crate::{
    AssetId, EditorService, ModuleRegistry,
    api::{ApiFailure, ApiRequest, ApiResponse, ClientId, OwnerHandle},
    catalog_types::{FileRecord, FileSignature, HeaderState},
    editor::mutation_json,
    library::locate::Phase,
    seed::IndexSeeder,
};
use luxforge_testbase::{
    Gate,
    paths::{fixture, jpeg, temp_dir},
};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    thread::JoinHandle,
};

/// The actor every client request here is made as.
const ACTOR: &str = "locate-client";

fn envelope(request_id: &str) -> Value {
    json!({"request_id": request_id, "actor": ACTOR})
}

/// One owner over a catalog in its own scratch directory.
struct Harness {
    dir: PathBuf,
    catalog: PathBuf,
    owner: OwnerHandle,
    join: Option<JoinHandle<()>>,
    client: ClientId,
}

impl Harness {
    /// A new catalog in a new scratch directory, and its owner.
    fn new(name: &str) -> Self {
        let dir = temp_dir(&format!("locate-{name}")).canonicalize().unwrap();
        let catalog = dir.join("catalog.sqlite");
        drop(EditorService::open(&catalog).unwrap());
        Self::start(dir, catalog)
    }

    fn start(dir: PathBuf, catalog: PathBuf) -> Self {
        let (owner, join) =
            OwnerHandle::start_with(&catalog, Arc::new(ModuleRegistry::builtin())).unwrap();
        let client = owner.register();
        Self {
            dir,
            catalog,
            owner,
            join: Some(join),
            client,
        }
    }

    fn stop(&mut self) {
        self.owner.stop();
        if let Some(join) = self.join.take() {
            join.join().unwrap();
        }
    }

    /// Stop the owner, do `between` with it stopped, and start a new one on the same catalog.
    fn restart(mut self, between: impl FnOnce(&Path)) -> Self {
        self.stop();
        between(&self.dir);
        let (dir, catalog) = (std::mem::take(&mut self.dir), self.catalog.clone());
        Self::start(dir, catalog)
    }

    fn send(&self, method: &str, params: Value) -> ApiResponse {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        self.owner
            .call(
                self.client,
                ApiRequest {
                    id: format!("{method}-{}", NEXT.fetch_add(1, Ordering::Relaxed)),
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

    /// Answer `method`, retrying through every preparation it asks for, as a client does.
    fn prepared(&self, method: &str, params: Value) -> Value {
        for _ in 0..4 {
            let response = self.send(method, params.clone());
            match response.error {
                None => return response.result.expect("a result"),
                Some(error) if error.code == "preparation-required" => {
                    let job = json!(error.job_id.expect("a preparation names its job"));
                    let settled = self.settle(&job);
                    assert_eq!(settled["status"], "ready", "{settled}");
                }
                Some(error) => panic!("{method}: {error:?}"),
            }
        }
        panic!("{method} kept asking for preparation");
    }

    /// Read a job until it leaves `queued` and `running`, as a client would.
    fn settle(&self, job: &Value) -> Value {
        luxforge_testbase::wait_for("a job to settle", || {
            let read = self.ok("job.read", json!({"job_id": job}));
            (!matches!(read["status"].as_str(), Some("queued" | "running"))).then_some(read)
        })
    }

    /// A copy of the fixture `name` (`s0/…`) at `path`, its folder made.
    fn copy(&self, name: &str, path: &Path) -> PathBuf {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::copy(fixture(&format!("s0/{name}")), path).unwrap();
        path.canonicalize().unwrap()
    }

    /// A copy of the fixture `name` (`s0/…`) at `path` in bytes no other copy has, so it develops
    /// into a photograph of its own rather than linking to another copy's
    /// ([`super::opening::distinct_copy`]).
    fn distinct(&self, name: &str, path: &Path) -> PathBuf {
        super::opening::distinct_copy(&fixture(&format!("s0/{name}")), path)
    }

    /// Develop the file at `path` and prepare its photograph, as a client opens a file.
    fn import(&self, path: &Path) -> AssetId {
        serde_json::from_value(super::opening::import(&self.owner, self.client, path)).unwrap()
    }

    fn state(&self, asset: &AssetId) -> Value {
        self.ok("asset.state", json!({"asset_id": asset}))
    }

    /// Commit a Basic exposure as the photograph's next entry.
    fn expose(&self, asset: &AssetId, exposure: f64) {
        let revision = self.state(asset)["revision"].as_u64().unwrap();
        self.ok(
            "edit.set-basic",
            json!({
                "asset_id": asset,
                "mutation": mutation_json(revision, &format!("expose-{revision}")),
                "exposure": exposure,
            }),
        );
    }

    /// Export the photograph's current entry to `name` in `out/` and answer the file's bytes.
    fn export(&self, asset: &AssetId, name: &str) -> Vec<u8> {
        let out = self.dir.join("out");
        fs::create_dir_all(&out).unwrap();
        let destination = out.join(name);
        let started = self.prepared(
            "export.jpeg",
            json!({"asset_id": asset, "destination": destination, "mutation": envelope(name)}),
        );
        let settled = self.settle(&started["job_id"]);
        assert_eq!(settled["status"], "ready", "{settled}");
        fs::read(destination).unwrap()
    }

    /// Start a Locate, answering the start.
    fn locate(&self, asset: &AssetId, path: &Path, request_id: &str) -> ApiResponse {
        self.send(
            "source.locate",
            json!({"asset_id": asset, "path": path, "mutation": envelope(request_id)}),
        )
    }

    /// Locate and wait for its job, answering the job as `job.read` reads it.
    fn located(&self, asset: &AssetId, path: &Path, request_id: &str) -> Value {
        let response = self.locate(asset, path, request_id);
        assert!(response.error.is_none(), "{:?}", response.error);
        self.settle(&response.result.unwrap()["job_id"])
    }

    /// Check `assets` and wait for the job, answering its rows as `{asset_id: availability}`.
    fn check(&self, assets: &[&AssetId]) -> Value {
        let started = self.ok(
            "source.check",
            json!({"targets": {"kind": "assets", "asset_ids": assets}}),
        );
        let settled = self.settle(&started["job_id"]);
        assert_eq!(settled["status"], "ready", "{settled}");
        assert_eq!(settled["kind"], "source-check");
        let rows = settled["result"]["rows"].as_array().unwrap();
        assert_eq!(rows.len(), assets.len());
        rows.iter()
            .map(|row| {
                assert!(row["checked_ms"].as_i64().unwrap() > 0);
                (
                    row["asset_id"].as_str().unwrap().to_owned(),
                    row["availability"].clone(),
                )
            })
            .collect::<serde_json::Map<_, _>>()
            .into()
    }

    /// The newest event's sequence.
    fn sequence(&self) -> u64 {
        self.ok("events.since", json!({"after": 0}))["current_sequence"]
            .as_u64()
            .unwrap()
    }

    fn events_after(&self, after: u64) -> Vec<Value> {
        self.ok("events.since", json!({"after": after}))["events"]
            .as_array()
            .unwrap()
            .clone()
    }

    /// The newest library change, with its rows.
    fn last_change(&self) -> Value {
        let journal = self.ok("library.journal", json!({"limit": 500}));
        let sequence = journal["changes"].as_array().unwrap().last().unwrap()["sequence"].clone();
        self.ok("library.inspect", json!({"sequence": sequence}))
    }

    fn post(&self, message: LibraryMessage) {
        self.owner
            .sender
            .send(OwnerMessage::Catalog(CatalogMessage::Library(message)))
            .unwrap();
    }

    /// Hold every library job dispatched from now on at `phase`, until the gate opens.
    fn hold_at(&self, phase: Phase) -> Arc<Gate> {
        let gate = Arc::new(Gate::new());
        gate.shut();
        let held = gate.clone();
        let hold: Hold = Arc::new(move |reached| {
            if reached == phase {
                held.pass();
            }
        });
        self.post(LibraryMessage::Hold(Some(hold)));
        gate
    }

    fn release_holds(&self) {
        self.post(LibraryMessage::Hold(None));
    }

    /// Run `arrange` on the owner, before the next request is served.
    fn on_owner(&self, arrange: impl FnOnce(&mut Owner) + Send + 'static) {
        self.post(LibraryMessage::Run(Box::new(arrange)));
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.stop();
        if !self.dir.as_os_str().is_empty() {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }
}

/// The locator a photograph's state names.
fn locator(state: &Value) -> PathBuf {
    PathBuf::from(state["asset"]["locator"].as_str().unwrap())
}

/// A job that settled as `failed` with `code`, answering its message.
fn failed(job: &Value, code: &str) -> String {
    assert_eq!(job["status"], "failed", "{job}");
    assert_eq!(job["error"]["code"], code, "{job}");
    job["error"]["message"].as_str().unwrap().to_owned()
}

/// The spec's first acceptance item: import and edit, close, move the original, reopen, Locate the
/// same bytes and export the original edits. A missing original is refused naming why until then;
/// the Locate is one library change naming the file, announced once, its history and identity
/// untouched, undone and redone with `library.undo` and `library.redo`, and nothing is written to
/// the original.
#[test]
fn a_moved_original_is_located_and_exports_the_same_edits() {
    let harness = Harness::new("moved");
    let original = harness.copy(
        "orientation-1.jpg",
        &harness.dir.join("card").join("DSC_0042.jpg"),
    );
    let bytes = fs::read(&original).unwrap();
    let asset = harness.import(&original);
    harness.expose(&asset, 0.75);
    let before = harness.state(&asset);
    let exported = harness.export(&asset, "before.jpg");

    // Closed, the original moves and is renamed; reopened, the catalog still looks where it was.
    let moved_dir = harness.dir.join("archive").join("2026");
    let harness = harness.restart(|dir| {
        fs::create_dir_all(dir.join("archive").join("2026")).unwrap();
        fs::rename(
            dir.join("card").join("DSC_0042.jpg"),
            dir.join("archive").join("2026").join("Lake.jpg"),
        )
        .unwrap();
    });
    let moved = moved_dir.join("Lake.jpg").canonicalize().unwrap();
    assert_eq!(locator(&harness.state(&asset)), original);

    // Develop and export refuse, naming why, with what a client needs to offer Locate.
    let refusal = harness.refused("source.prepare", json!({"asset_id": asset}));
    assert_eq!(refusal.code, "source-unavailable", "{refusal:?}");
    assert!(
        refusal.message.contains("DSC_0042.jpg is missing from"),
        "{}",
        refusal.message
    );
    let data = refusal.data.clone().unwrap();
    assert_eq!(data["availability"], "missing");
    assert_eq!(data["locator"], json!(original));
    let export = harness.refused(
        "export.jpeg",
        json!({"asset_id": asset, "destination": harness.dir.join("never.jpg"), "mutation": envelope("never")}),
    );
    assert_eq!(export.code, "source-unavailable");
    assert_eq!(export.data.unwrap()["availability"], "missing");
    assert_eq!(
        harness.ok("catalog.info", json!({}))["counts"]["unavailable"],
        1,
        "the refusal recorded what it found"
    );

    // Locate the same bytes where they are now.
    let sequence = harness.sequence();
    let job = harness.located(&asset, &moved, "locate-1");
    assert_eq!(job["status"], "ready", "{job}");
    assert_eq!(job["kind"], "source-locate");
    assert_eq!(job["asset_id"], json!(asset));
    let answer = &job["result"];
    assert_eq!(answer["outcome"], "applied", "{answer}");
    assert_eq!(answer["items"], 1);
    let change = answer["change"].as_u64().unwrap();

    let after = harness.state(&asset);
    assert_eq!(locator(&after), moved);
    assert_eq!(
        after["asset"]["source_root"],
        json!(moved_dir.canonicalize().unwrap())
    );
    assert_eq!(after["asset"]["id"], before["asset"]["id"]);
    assert_eq!(
        after["asset"]["fingerprint"],
        before["asset"]["fingerprint"]
    );
    assert_eq!(
        after["revision"], before["revision"],
        "history did not move"
    );
    assert_eq!(after["current_entry"], before["current_entry"]);
    assert_eq!(
        harness.ok("catalog.info", json!({}))["counts"]["unavailable"],
        0
    );

    // One library change naming the file, by the client's actor, announced once.
    let detail = harness.last_change();
    assert_eq!(detail["change"]["sequence"], change);
    assert_eq!(detail["change"]["method"], "source.locate");
    assert_eq!(detail["change"]["actor"], ACTOR);
    assert_eq!(detail["change"]["label"], "Located Lake.jpg");
    let rows = detail["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0]["item"],
        json!({"kind": "asset-source", "asset_id": asset})
    );
    assert_eq!(rows[0]["before"]["locator"], json!(original));
    assert_eq!(rows[0]["after"]["locator"], json!(moved));
    assert_eq!(
        rows[0]["after"]["source_folder"],
        json!(moved_dir.canonicalize().unwrap())
    );
    let events = harness.events_after(sequence);
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0]["method"], "source.locate");
    assert_eq!(events[0]["library_sequence"], change);
    assert_eq!(
        events[0]["asset_id"],
        json!(asset),
        "a change to one photograph's original names it, so a client showing it reads it again"
    );
    assert_eq!(events[0].get("revision"), None, "its history did not move");

    // The original edits export exactly as before the move.
    assert_eq!(harness.export(&asset, "after.jpg"), exported);

    // Undo points the photograph where it was, and redo where it is.
    let undone = harness.ok("library.undo", json!({"mutation": envelope("undo-1")}));
    assert_eq!(undone["outcome"], "applied");
    assert_eq!(locator(&harness.state(&asset)), original);
    assert_eq!(
        harness
            .refused("source.prepare", json!({"asset_id": asset}))
            .code,
        "source-unavailable"
    );
    let redone = harness.ok("library.redo", json!({"mutation": envelope("redo-1")}));
    assert_eq!(redone["outcome"], "applied");
    assert_eq!(locator(&harness.state(&asset)), moved);

    // A retry of the Locate is answered with its first answer; after a restart, with its change as
    // a finished job, reading nothing, though the file has moved again.
    let retry = harness.locate(&asset, &moved, "locate-1").result.unwrap();
    assert_eq!(retry["deduplicated"], true);
    assert_eq!(
        fs::read(&moved).unwrap(),
        bytes,
        "the original was only read"
    );
    let harness = harness.restart(|dir| {
        fs::rename(
            dir.join("archive").join("2026").join("Lake.jpg"),
            dir.join("Lake.jpg"),
        )
        .unwrap();
    });
    let retry = harness.locate(&asset, &moved, "locate-1").result.unwrap();
    assert_eq!(retry["status"], "ready", "{retry}");
    let job = harness.ok("job.read", json!({"job_id": retry["job_id"]}));
    assert_eq!(job["result"]["change"], change, "{job}");
    assert_eq!(job["result"]["deduplicated"], true);
}

/// A byte-identical copy in another folder is the photograph's original too: the Locate keeps the
/// photograph's identity and history, and the original it was pointing at is left as it was.
#[test]
fn a_byte_identical_copy_is_located_without_touching_either_file() {
    let harness = Harness::new("copy");
    let original = harness.copy("orientation-1.jpg", &harness.dir.join("a").join("one.jpg"));
    let copy = harness.copy("orientation-1.jpg", &harness.dir.join("b").join("copy.jpg"));
    let asset = harness.import(&original);
    harness.expose(&asset, -0.5);
    let before = harness.state(&asset);
    let pixel =
        harness.prepared("render.sample", json!({"asset_id": asset, "x": 7, "y": 9}))["rgba"]
            .clone();

    let job = harness.located(&asset, &copy, "copy-1");
    assert_eq!(job["result"]["outcome"], "applied", "{job}");
    let after = harness.state(&asset);
    assert_eq!(locator(&after), copy);
    assert_eq!(after["asset"]["id"], before["asset"]["id"]);
    assert_eq!(after["current_entry"], before["current_entry"]);
    assert_eq!(
        harness.prepared("render.sample", json!({"asset_id": asset, "x": 7, "y": 9}))["rgba"],
        pixel
    );
    // Locating it where it already is changes nothing.
    let again = harness.located(&asset, &copy, "copy-2");
    assert_eq!(again["result"]["outcome"], "no-op", "{again}");
    for path in [&original, &copy] {
        assert_eq!(fs::read(path).unwrap(), fs::read(jpeg()).unwrap());
    }
}

/// What is not the photograph's original is refused and changes nothing: a relative path, a
/// folder, a path with nothing there and a file of another length before anything is read; a
/// different photograph of the same length, and the original rewritten in place, once its
/// fingerprint is streamed.
#[test]
fn what_is_not_the_original_is_refused_and_changes_nothing() {
    let harness = Harness::new("refused");
    let original = harness.copy(
        "orientation-1.jpg",
        &harness.dir.join("a").join("DSC_0042.jpg"),
    );
    let asset = harness.import(&original);
    let before = harness.state(&asset);
    let sequence = harness.sequence();
    let journal = harness.ok("library.journal", json!({}));

    let start = |path: &Path, request: &str| harness.locate(&asset, path, request).error.unwrap();
    let relative = start(Path::new("a/DSC_0042.jpg"), "relative");
    assert_eq!(relative.code, "validation", "{relative:?}");
    let folder = start(&harness.dir.join("a"), "folder");
    assert_eq!(folder.code, "validation", "{folder:?}");
    let nothing = start(&harness.dir.join("a").join("gone.jpg"), "nothing");
    assert_eq!(nothing.code, "read-error", "{nothing:?}");
    // Another photograph with the same name and another length.
    let other = harness.copy("greyscale.jpg", &harness.dir.join("b").join("DSC_0042.jpg"));
    let shorter = start(&other, "shorter");
    assert_eq!(shorter.code, "source-unavailable", "{shorter:?}");
    assert!(shorter.message.contains("bytes"), "{}", shorter.message);
    // The original with its metadata rewritten: longer.
    let rewritten = harness.dir.join("c").join("DSC_0042.jpg");
    fs::create_dir_all(rewritten.parent().unwrap()).unwrap();
    let mut longer = fs::read(&original).unwrap();
    longer.extend_from_slice(b"rewritten metadata");
    fs::write(&rewritten, longer).unwrap();
    assert_eq!(start(&rewritten, "longer").code, "source-unavailable");

    // A different photograph of exactly the original's length, by the same name: its bytes differ.
    let same_length = harness.copy(
        "orientation-2.jpg",
        &harness.dir.join("d").join("DSC_0042.jpg"),
    );
    let job = harness.located(&asset, &same_length, "same-length");
    assert!(
        failed(&job, "source-unavailable").contains("its bytes differ"),
        "{job}"
    );
    // The original with one byte changed in place.
    let changed = harness.dir.join("e").join("DSC_0042.jpg");
    fs::create_dir_all(changed.parent().unwrap()).unwrap();
    let mut flipped = fs::read(&original).unwrap();
    let middle = flipped.len() / 2;
    flipped[middle] ^= 0xff;
    fs::write(&changed, flipped).unwrap();
    let job = harness.located(&asset, &changed.canonicalize().unwrap(), "changed");
    failed(&job, "source-unavailable");

    assert_eq!(harness.state(&asset), before);
    assert_eq!(harness.ok("library.journal", json!({})), journal);
    assert!(harness.events_after(sequence).is_empty());
}

/// A file another photograph already names is refused, naming that photograph, and neither is
/// merged into the other: both keep their identity, history and original.
#[test]
fn a_file_another_photograph_names_is_refused_without_a_merge() {
    let harness = Harness::new("claimed");
    let a = harness.distinct("orientation-1.jpg", &harness.dir.join("a.jpg"));
    let b = harness.distinct("orientation-1.jpg", &harness.dir.join("b.jpg"));
    let first = harness.import(&a);
    let second = harness.import(&b);
    harness.expose(&second, 1.0);
    let (first_state, second_state) = (harness.state(&first), harness.state(&second));

    let refusal = harness.locate(&first, &b, "claimed").error.unwrap();
    assert_eq!(refusal.code, "conflict", "{refusal:?}");
    assert!(
        refusal.message.contains(second.as_str()),
        "{}",
        refusal.message
    );
    assert_eq!(refusal.data.unwrap()["asset_id"], json!(second));
    assert_eq!(harness.state(&first), first_state);
    assert_eq!(harness.state(&second), second_state);
}

/// A file modified while it is verified is refused, and so is one modified, or taken by another
/// photograph, after it was verified and before the owner commits; an edit committed meanwhile is
/// unaffected, since a Locate moves no history.
#[test]
fn a_file_changed_around_its_verification_is_refused() {
    let harness = Harness::new("changing");
    let original = harness.copy("orientation-1.jpg", &harness.dir.join("a").join("one.jpg"));
    let asset = harness.import(&original);
    let before = harness.state(&asset);
    let candidate =
        |name: &str| harness.copy("orientation-1.jpg", &harness.dir.join("b").join(name));
    let touch = |path: &Path| {
        let file = fs::OpenOptions::new().append(true).open(path).unwrap();
        let len = file.metadata().unwrap().len();
        file.set_len(len + 1).unwrap();
        file.set_len(len).unwrap();
    };

    // Touched after every byte was read, before the signature is taken again.
    let during = candidate("during.jpg");
    let gate = harness.hold_at(Phase::Hashed);
    let job = harness.locate(&asset, &during, "during").result.unwrap();
    gate.wait_reached(1, "the verification");
    touch(&during);
    gate.open();
    let settled = harness.settle(&job["job_id"]);
    assert!(
        failed(&settled, "conflict").contains("while it was being verified"),
        "{settled}"
    );

    // Touched after it was verified, before the owner commits.
    let after = candidate("after.jpg");
    let gate = harness.hold_at(Phase::Verified);
    let job = harness.locate(&asset, &after, "after").result.unwrap();
    gate.wait_reached(1, "the verification");
    touch(&after);
    gate.open();
    let settled = harness.settle(&job["job_id"]);
    assert!(
        failed(&settled, "conflict").contains("after it was verified"),
        "{settled}"
    );

    // Taken by another photograph after it was verified — that photograph relinked to it — so
    // refused, naming it. An edit committed while the Locate was held stands.
    let other = harness.import(&harness.distinct(
        "orientation-1.jpg",
        &harness.dir.join("c").join("other.jpg"),
    ));
    let taken = candidate("taken.jpg");
    let gate = harness.hold_at(Phase::Verified);
    let job = harness.locate(&asset, &taken, "taken").result.unwrap();
    gate.wait_reached(1, "the verification");
    let (path, signature) = EditorService::request_signature(&taken).unwrap();
    let source = crate::library::locate::source_value(
        &path,
        crate::index::volume_of(&path, 0).unwrap().id,
        signature.file_identity(),
    );
    let relinked = other.clone();
    harness.on_owner(move |owner| {
        crate::editor::write(&mut owner.service.connection, |tx| {
            crate::editor::library_rows::set_asset_source(tx, &relinked, &source)?;
            Ok(())
        })
        .unwrap();
    });
    harness.expose(&asset, 0.25);
    gate.open();
    let settled = harness.settle(&job["job_id"]);
    assert!(
        failed(&settled, "conflict").contains(other.as_str()),
        "{settled}"
    );
    harness.release_holds();
    assert_eq!(locator(&harness.state(&asset)), original);

    // Verified and committed while an edit lands: both stand.
    let gate = harness.hold_at(Phase::Verified);
    let fine = candidate("fine.jpg");
    let job = harness.locate(&asset, &fine, "fine").result.unwrap();
    gate.wait_reached(1, "the verification");
    harness.expose(&asset, 0.5);
    let edited = harness.state(&asset);
    gate.open();
    let settled = harness.settle(&job["job_id"]);
    assert_eq!(settled["result"]["outcome"], "applied", "{settled}");
    harness.release_holds();
    let state = harness.state(&asset);
    assert_eq!(locator(&state), fine);
    assert_eq!(state["revision"], edited["revision"]);
    assert_eq!(state["current_entry"], edited["current_entry"]);
    assert_ne!(state["revision"], before["revision"]);
}

/// Cancelling a verification and a commit that fails each leave the photograph where it was, with
/// nothing recorded; the same file is then located.
#[test]
fn a_cancel_and_a_failed_commit_leave_the_last_state() {
    let harness = Harness::new("cancel");
    let original = harness.copy("orientation-1.jpg", &harness.dir.join("a").join("one.jpg"));
    let copy = harness.copy("orientation-1.jpg", &harness.dir.join("b").join("copy.jpg"));
    let asset = harness.import(&original);
    let before = harness.state(&asset);
    let journal = harness.ok("library.journal", json!({}));
    let sequence = harness.sequence();

    // Cancelled while it streams the file.
    let gate = harness.hold_at(Phase::Hashing);
    let job = harness.locate(&asset, &copy, "cancelled").result.unwrap();
    gate.wait_reached(1, "the verification");
    let read = harness.ok("job.read", json!({"job_id": job["job_id"]}));
    assert_eq!(read["status"], "running");
    let activity = harness.ok("activity.list", json!({}));
    assert!(
        activity["active"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["kind"] == "source.locate"
                && entry["job_id"] == job["job_id"]
                && entry["detail"] == "copy.jpg"),
        "{activity}"
    );
    let cancelled = harness.ok("job.cancel", json!({"job_id": job["job_id"]}));
    assert_eq!(cancelled["kind"], "source-locate");
    gate.open();
    let settled = harness.settle(&job["job_id"]);
    assert_eq!(settled["status"], "cancelled", "{settled}");
    harness.release_holds();

    // A commit the catalog refuses.
    harness.on_owner(|owner| {
        owner
            .service
            .connection
            .execute_batch(
                "CREATE TEMP TRIGGER refuse_locate BEFORE UPDATE OF locator ON assets
                 BEGIN SELECT RAISE(ABORT, 'injected'); END;",
            )
            .unwrap();
    });
    let job = harness.located(&asset, &copy, "refused");
    assert_eq!(job["status"], "failed", "{job}");
    assert_eq!(harness.state(&asset), before);
    assert_eq!(harness.ok("library.journal", json!({})), journal);
    assert!(harness.events_after(sequence).is_empty());

    harness.on_owner(|owner| {
        owner
            .service
            .connection
            .execute_batch("DROP TRIGGER refuse_locate;")
            .unwrap();
    });
    let job = harness.located(&asset, &copy, "located");
    assert_eq!(job["result"]["outcome"], "applied", "{job}");
    assert_eq!(locator(&harness.state(&asset)), copy);
}

/// A photograph whose file moved within its volume is found again by its file identity in the
/// index, confirmed by its fingerprint and relinked by the check itself as one library change by
/// `system`; a file the index lists under the identity whose bytes changed is not taken, a missing
/// one stays missing and another file in a photograph's place is changed. Everything is recorded
/// with the time it was checked and announced once.
#[test]
fn a_check_finds_a_same_volume_move_by_identity_and_nothing_else() {
    let dir = temp_dir("locate-identity").canonicalize().unwrap();
    let catalog = dir.join("catalog.sqlite");
    let card = dir.join("card");
    let archive = dir.join("archive");
    fs::create_dir_all(&card).unwrap();
    fs::create_dir_all(&archive).unwrap();
    let copy = |name: &str, fixture_name: &str| {
        let path = card.join(name);
        fs::copy(fixture(&format!("s0/{fixture_name}")), &path).unwrap();
        path.canonicalize().unwrap()
    };
    let paths = [
        copy("moved.jpg", "orientation-1.jpg"),
        copy("rewritten.jpg", "orientation-2.jpg"),
        copy("deleted.jpg", "orientation-3.jpg"),
        copy("replaced.jpg", "orientation-4.jpg"),
        copy("still.jpg", "orientation-5.jpg"),
    ];
    let (catalog_id, assets) = {
        let mut service = EditorService::open(&catalog).unwrap();
        let assets: Vec<AssetId> = paths
            .iter()
            .map(|path| service.import(path).unwrap().asset.id)
            .collect();
        (service.catalog_id().to_owned(), assets)
    };
    // Moved within the volume, which the index lists under its new path; rewritten in place after
    // a move, so its identity holds but its bytes do not; deleted; replaced by another file.
    let moved = archive.join("renamed.jpg");
    fs::rename(&paths[0], &moved).unwrap();
    let rewritten = archive.join("rewritten.jpg");
    fs::rename(&paths[1], &rewritten).unwrap();
    let mut bytes = fs::read(&rewritten).unwrap();
    bytes[100] ^= 0xff;
    fs::OpenOptions::new()
        .write(true)
        .open(&rewritten)
        .and_then(|mut file| std::io::Write::write_all(&mut file, &bytes))
        .unwrap();
    fs::remove_file(&paths[2]).unwrap();
    fs::remove_file(&paths[3]).unwrap();
    fs::copy(fixture("s0/orientation-6.jpg"), &paths[3]).unwrap();
    let volume = crate::index::volume_of(&archive, 1).unwrap().id;
    let listed = |path: &Path| {
        let path = path.canonicalize().unwrap();
        FileRecord {
            path: path.clone(),
            folder: path.parent().unwrap().to_path_buf(),
            name: path.file_name().unwrap().to_string_lossy().into(),
            volume_id: volume.clone(),
            signature: FileSignature::of(&path.metadata().unwrap()),
            kind: crate::SourceTag::Jpeg,
            header: HeaderState::Pending,
            last_seen_ms: 1,
        }
    };
    let mut seeder = IndexSeeder::create(&catalog, &catalog_id).unwrap();
    seeder.files(&[listed(&moved), listed(&rewritten)]).unwrap();
    seeder.finish().unwrap();

    let harness = Harness::start(dir, catalog);
    let sequence = harness.sequence();
    let asset_refs: Vec<&AssetId> = assets.iter().collect();
    let rows = harness.check(&asset_refs);
    assert_eq!(
        rows,
        json!({
            assets[0].as_str(): "available",
            assets[1].as_str(): "missing",
            assets[2].as_str(): "missing",
            assets[3].as_str(): "changed",
            assets[4].as_str(): "available",
        })
    );
    assert_eq!(
        locator(&harness.state(&assets[0])),
        moved.canonicalize().unwrap()
    );
    assert_eq!(locator(&harness.state(&assets[1])), paths[1]);

    // One library change by the system, under the check.
    let detail = harness.last_change();
    assert_eq!(detail["change"]["actor"], "system");
    assert_eq!(detail["change"]["method"], "source.check");
    assert_eq!(detail["change"]["label"], "Found renamed.jpg again");
    assert_eq!(detail["rows"].as_array().unwrap().len(), 1);
    assert_eq!(detail["rows"][0]["item"]["asset_id"], json!(assets[0]));
    let events = harness.events_after(sequence);
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0]["method"], "source.check");
    assert_eq!(events[0]["library_sequence"], detail["change"]["sequence"]);
    // The unavailable count follows what was recorded, and a check that changes nothing announces
    // nothing.
    assert_eq!(
        harness.ok("catalog.info", json!({}))["counts"]["unavailable"],
        3
    );
    let sequence = harness.sequence();
    assert_eq!(harness.check(&asset_refs), rows);
    assert!(harness.events_after(sequence).is_empty());
    // The relink is the system's: a client's undo does not take it back.
    let undone = harness.ok("library.undo", json!({"mutation": envelope("undo")}));
    assert_eq!(undone["outcome"], "no-op");
}

/// A volume mounted again after another disk may be given another device number, which renumbers
/// the file identity of every file on it: a check finds each photograph's own file still at its
/// locator, confirms it by its fingerprint and records its identity now, as one change by the
/// system, so the photograph is available again without a Locate.
#[test]
fn a_check_locates_an_original_whose_file_identity_was_renumbered() {
    let harness = Harness::new("renumbered");
    let path = harness.copy(
        "orientation-1.jpg",
        &harness.dir.join("Photos").join("one.jpg"),
    );
    let asset = harness.import(&path);
    let catalog = harness.catalog.clone();
    let recorded = std::cell::RefCell::new(String::new());
    let harness = harness.restart(|_| {
        let connection = rusqlite::Connection::open(&catalog).unwrap();
        let identity: String = connection
            .query_row(
                "SELECT file_identity FROM assets WHERE id = ?1",
                [asset.as_str()],
                |row| row.get(0),
            )
            .unwrap();
        let inode = identity.rsplit(':').next().unwrap().to_owned();
        connection
            .execute(
                "UPDATE assets SET file_identity = ?1 WHERE id = ?2",
                [format!("unix:1:{inode}"), asset.to_string()],
            )
            .unwrap();
        *recorded.borrow_mut() = identity;
    });
    let sequence = harness.sequence();
    assert_eq!(
        harness.check(&[&asset]),
        json!({asset.as_str(): "available"})
    );
    assert_eq!(locator(&harness.state(&asset)), path);
    let detail = harness.last_change();
    assert_eq!(
        (&detail["change"]["actor"], &detail["change"]["label"]),
        (&json!("system"), &json!("Found one.jpg again"))
    );
    assert_eq!(
        detail["rows"][0]["after"]["file_identity"],
        json!(*recorded.borrow()),
        "the identity it has now"
    );
    assert_eq!(harness.events_after(sequence).len(), 1);
    // The photograph exports as before.
    assert!(!harness.export(&asset, "after.jpg").is_empty());
}

/// Availability across a disk image, on the Mac: a detached image makes every photograph on it
/// offline with one look, Develop and export refuse naming the volume, a Locate onto the image is
/// refused when the image goes while it verifies, and re-attached, everything is available again
/// and a photograph is located onto the image, another volume, keeping its identity and history.
#[cfg(target_os = "macos")]
#[test]
fn a_detached_disk_image_makes_its_photographs_offline() {
    use crate::library::test_disk::DiskImage;

    let harness = Harness::new("disk-image");
    let mut disk = DiskImage::create(&harness.dir, "LuxforgeTest");
    let mount = disk.mount.canonicalize().unwrap();
    let on_image = [
        harness.copy("orientation-1.jpg", &mount.join("DCIM").join("one.jpg")),
        harness.copy("orientation-2.jpg", &mount.join("DCIM").join("two.jpg")),
    ];
    let local = harness.copy(
        "orientation-3.jpg",
        &harness.dir.join("local").join("three.jpg"),
    );
    let images: Vec<AssetId> = on_image.iter().map(|path| harness.import(path)).collect();
    let here = harness.import(&local);
    harness.expose(&images[0], 0.5);
    let all = [&images[0], &images[1], &here];
    let available = json!({
        images[0].as_str(): "available",
        images[1].as_str(): "available",
        here.as_str(): "available",
    });
    assert_eq!(harness.check(&all), available);

    // Detached: both of its photographs are offline, the other is not.
    disk.detach();
    assert_eq!(
        harness.check(&all),
        json!({
            images[0].as_str(): "offline",
            images[1].as_str(): "offline",
            here.as_str(): "available",
        })
    );
    assert_eq!(
        harness.ok("catalog.info", json!({}))["counts"]["unavailable"],
        2
    );
    // Develop and export refuse naming the volume, with the volume in the data.
    for refusal in [
        harness.refused("source.prepare", json!({"asset_id": images[0]})),
        harness.refused(
            "export.jpeg",
            json!({"asset_id": images[0], "destination": harness.dir.join("x.jpg"), "mutation": envelope("offline-export")}),
        ),
        harness.refused("render.sample", json!({"asset_id": images[1], "x": 0, "y": 0})),
    ] {
        assert_eq!(refusal.code, "source-unavailable", "{refusal:?}");
        assert_eq!(
            refusal.message, "the volume LuxforgeTest is not connected",
            "{refusal:?}"
        );
        let data = refusal.data.unwrap();
        assert_eq!(data["availability"], "offline");
        assert_eq!(data["volume"]["label"], "LuxforgeTest");
        assert_eq!(data["volume"]["mount_point"], json!(mount));
    }

    // Re-attached: available again, and the edits export.
    disk.attach();
    assert_eq!(harness.check(&all), available);
    assert!(!harness.export(&images[0], "edited.jpg").is_empty());

    // A Locate onto the image, another volume: refused when the image is detached while the file
    // is verified, changing nothing.
    let target = harness.copy(
        "orientation-3.jpg",
        &mount.join("Archive").join("three.jpg"),
    );
    let before = harness.state(&here);
    let gate = harness.hold_at(Phase::Hashed);
    let job = harness.locate(&here, &target, "unplugged").result.unwrap();
    gate.wait_reached(1, "the verification");
    disk.detach();
    gate.open();
    let settled = harness.settle(&job["job_id"]);
    assert_eq!(settled["status"], "failed", "{settled}");
    harness.release_holds();
    assert_eq!(harness.state(&here), before);

    // Re-attached, the Locate onto the image keeps the photograph and records its new volume.
    disk.attach();
    let job = harness.located(&here, &target, "onto-image");
    assert_eq!(job["result"]["outcome"], "applied", "{job}");
    let after = harness.state(&here);
    assert_eq!(locator(&after), target);
    assert_eq!(after["asset"]["id"], before["asset"]["id"]);
    assert_eq!(after["current_entry"], before["current_entry"]);
    let detail = harness.last_change();
    assert_ne!(
        detail["rows"][0]["before"]["volume_id"], detail["rows"][0]["after"]["volume_id"],
        "{detail}"
    );
    disk.detach();
    assert_eq!(
        harness.check(&all),
        json!({
            images[0].as_str(): "offline",
            images[1].as_str(): "offline",
            here.as_str(): "offline",
        })
    );
}
