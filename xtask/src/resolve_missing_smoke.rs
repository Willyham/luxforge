//! The `resolve-missing` smoke scenario: Select's Missing originals over a catalog whose originals
//! were reorganized, in the real editor at 1440 × 900 (TASK-023).
//!
//! The run first makes its photographs: real generated JPEGs (`generate-catalog --images`), copied
//! onto a disk image labelled "Photos SSD" (on macOS, made with `hdiutil` and attached with
//! `-nobrowse` at a mount point inside the run, so nothing appears on the desktop; elsewhere a
//! scratch folder of that name stands in for it), onto a second image, "Old SSD", and into a
//! scratch card-dump folder, and develops them into a new catalog with `catalog.import`. It then
//! reorganizes them as a photographer would, never touching anything outside the run:
//!
//! - `2026/2026-09 Konstanz` on Photos SSD moves within the image to `Archive/Photographs/…`: five
//!   files as they were, one rewritten (other bytes at its name), one duplicated into
//!   `Archive/Backup`, and one deleted, a copy of it kept outside the archive under another name;
//! - `2026/2026-09 Lake` goes deep into a tree of 20,000 scratch folders, which a search is still
//!   walking when it is stopped;
//! - the card-dump folder is deleted, and Old SSD is detached.
//!
//! `source.check` records every original missing, and the runner asks the core, through its own
//! client, for the answers the frames are checked against (`resolve-missing-expected.json`): what
//! `source.missing` lists and what `source.find` answers for Konstanz searched in the archive.
//! The editor then opens that catalog with nothing open. Its frames, in [`plan`] order: Develop;
//! `G` showing Select; Missing originals from the sources panel (the groups and their reasons);
//! Find in a folder… on Konstanz (every row's result); the Needs you filter; All again; Choose… the
//! second of the duplicated file's copies; a row selected (the Info panel); Find in a folder… on
//! Lake, stopped while it walks (nothing changed); Relink (exactly the verified pairs); and
//! Locate… of the deleted photograph's copy. After the run the runner reads the catalog the editor
//! left through its own client (`resolve-missing-after.json`): the relinked photographs point at
//! their new files, the located one at its copy, and every other where it was; the journal holds
//! exactly the relink and the Locate. A replay checks the recorded frames against both files.
use crate::{
    generate_catalog,
    scenario::{Checked, Checks, Frame, Launch, Plan, Run, Step, plan::only},
    smoke::Scenario,
    *,
};
use luxforge_core::{ApiRequest, ClientId, EditorService, OwnerHandle};
use luxforge_evidence::{self as script, MissingFilterStep, MissingStep, SelectStep};
use std::process::Command;

pub const SCENARIO: &str = "resolve-missing";
/// What `reproduce.md` says the run does before it launches.
pub const NOTE: &str = "The run first generates real JPEGs with `cargo xtask generate-catalog \
    --images 60 --seed 1`, copies them onto two disk images made with `hdiutil` and attached with \
    `-nobrowse` inside the run (a scratch folder elsewhere than macOS) and into a scratch folder, \
    develops them into `generated/catalog.sqlite` with `catalog.import`, reorganizes them (moved, \
    rewritten, duplicated, deleted, a drive detached), records them missing with `source.check`, \
    asks the core for the answers the frames are checked against (`resolve-missing-expected.json`) \
    and launches the editor over that catalog with `--catalog`; after the run it reads the catalog \
    the editor left (`resolve-missing-after.json`) and detaches the images.";
/// Where the run writes its photographs, images and catalog.
pub const GENERATED: &str = "generated";
pub const EXPECTED: &str = "resolve-missing-expected.json";
pub const AFTER: &str = "resolve-missing-after.json";
const SEED: u64 = 1;
const IMAGES: u32 = 60;
/// The folders developed from, as their groups are named.
const KONSTANZ: &str = "2026-09 Konstanz";
const LAKE: &str = "2026-09 Lake";
const WINTER: &str = "2025 Winter";
const DUMP: &str = "2026-09-10";
/// The photographs, by their originals' names.
const FOUND: [&str; 5] = [
    "DSC_0101.JPG",
    "DSC_0102.JPG",
    "DSC_0103.JPG",
    "DSC_0104.JPG",
    "DSC_0105.JPG",
];
const REWRITTEN: &str = "DSC_0106.JPG";
const DUPLICATED: &str = "DSC_0107.JPG";
const DELETED: &str = "DSC_0108.JPG";
const LAKE_FILES: [&str; 3] = ["DSCF0201.JPG", "DSCF0202.JPG", "DSCF0203.JPG"];
const DUMP_FILES: [&str; 2] = ["L1000301.JPG", "L1000302.JPG"];
const WINTER_FILES: [&str; 2] = ["IMG_0401.JPG", "IMG_0402.JPG"];
/// The duplicated file's copy Choose… picks, as its menu lists them: in the order `source.find`
/// answered them.
const CHOSEN: usize = 1;
/// The folders the deep tree holds: 200 of 100 each.
const TREE: (usize, usize) = (200, 100);

/// Every frame, in order: Konstanz is searched in `archive`, Lake in `tree`, and the deleted
/// photograph located at `rescued`.
pub fn plan(archive: &str, tree: &str, rescued: &str) -> Plan {
    let select = |name: &str, step: SelectStep| Step::new(name, script::Step::Select(step));
    let resolve = |name: &str, step: MissingStep| Step::new(name, script::Step::Missing(step));
    Plan::new(vec![
        Step::opened("opened"),
        Step::new("select", script::Step::key("g")),
        select("missing", SelectStep::Source("Missing originals".into())),
        resolve(
            "found",
            MissingStep::Find {
                group: KONSTANZ.into(),
                folder: archive.into(),
                stop: false,
            },
        )
        .status_starts("Searched "),
        resolve(
            "needs-you",
            MissingStep::Filter(MissingFilterStep::NeedsYou),
        ),
        resolve("all", MissingStep::Filter(MissingFilterStep::All)),
        resolve(
            "chosen",
            MissingStep::Choose {
                file: DUPLICATED.into(),
                index: CHOSEN,
            },
        )
        .status_starts("Chose "),
        resolve("row", MissingStep::Row(FOUND[0].into())),
        resolve(
            "stopped",
            MissingStep::Find {
                group: LAKE.into(),
                folder: tree.into(),
                stop: true,
            },
        )
        .status_starts("Stopped searching "),
        resolve("relinked", MissingStep::Relink).status_starts("Relinked 6 originals"),
        resolve(
            "located",
            MissingStep::Locate {
                file: DELETED.into(),
                path: rescued.into(),
            },
        )
        .status_starts(format!("Located {DELETED}")),
    ])
}

/// One request through the runner's own client.
fn ask(owner: &OwnerHandle, client: ClientId, method: &str, params: Value) -> Result<Value> {
    let response = owner
        .call(
            client,
            ApiRequest {
                id: format!("resolve-missing-smoke-{method}"),
                method: method.into(),
                params,
                token: None,
            },
        )
        .map_err(|error| format!("{method}: {error}"))?;
    match response.error {
        Some(error) => Err(format!("{method}: {}: {}", error.code, error.message).into()),
        None => Ok(response.result.unwrap_or(Value::Null)),
    }
}

/// Read a job until it ends.
fn settle(owner: &OwnerHandle, client: ClientId, job: &Value) -> Result<Value> {
    let started = std::time::Instant::now();
    loop {
        let read = ask(owner, client, "job.read", json!({ "job_id": job }))?;
        if !matches!(read["status"].as_str(), Some("queued" | "running")) {
            return Ok(read);
        }
        ensure(
            started.elapsed() < std::time::Duration::from_secs(120),
            format!("A job did not end: {read}"),
        )?;
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// `body` through an owner of the runner's own over `catalog`.
fn with_owner<T>(
    catalog: &Path,
    body: impl FnOnce(&OwnerHandle, ClientId) -> Result<T>,
) -> Result<T> {
    let (owner, join) = OwnerHandle::start(catalog)
        .map_err(|error| format!("the core cannot open the catalog: {error}"))?;
    let client = owner.register();
    let outcome = body(&owner, client);
    owner.stop();
    let _ = join.join();
    outcome
}

/// A disk image the run made, attached at a mount point inside the run, and detached when dropped.
struct DiskImage {
    image: PathBuf,
    mount: PathBuf,
    attached: bool,
}

impl DiskImage {
    /// Make an HFS+ image labelled `label` and attach it at `mount`, without showing it anywhere.
    /// Off macOS, a scratch folder at `mount` stands in for it.
    fn create(dir: &Path, label: &str) -> Result<Self> {
        let mount = dir.join("volumes").join(label);
        fs::create_dir_all(&mount)?;
        let image = dir.join(format!("{label}.dmg"));
        let mut disk = Self {
            image,
            mount,
            attached: false,
        };
        if cfg!(target_os = "macos") {
            let status = Command::new("hdiutil")
                .args([
                    "create", "-quiet", "-size", "32m", "-fs", "HFS+", "-volname", label,
                ])
                .arg(&disk.image)
                .status()?;
            ensure(status.success(), format!("hdiutil create: {status}"))?;
            disk.attach()?;
        }
        Ok(disk)
    }

    fn attach(&mut self) -> Result {
        let status = Command::new("hdiutil")
            .args(["attach", "-quiet", "-nobrowse", "-mountpoint"])
            .arg(&self.mount)
            .arg(&self.image)
            .status()?;
        ensure(status.success(), format!("hdiutil attach: {status}"))?;
        self.attached = true;
        Ok(())
    }

    /// Detach the image, or, off macOS, take the stand-in folder away as an unplugged drive is.
    fn detach(&mut self) -> Result {
        if !cfg!(target_os = "macos") {
            fs::rename(&self.mount, self.mount.with_extension("unplugged"))?;
            return Ok(());
        }
        let status = Command::new("hdiutil")
            .args(["detach", "-quiet", "-force"])
            .arg(&self.mount)
            .status()?;
        ensure(status.success(), format!("hdiutil detach: {status}"))?;
        self.attached = false;
        Ok(())
    }

    fn root(&self) -> Result<PathBuf> {
        Ok(self.mount.canonicalize()?)
    }
}

impl Drop for DiskImage {
    fn drop(&mut self) {
        if self.attached {
            let _ = Command::new("hdiutil")
                .args(["detach", "-quiet", "-force"])
                .arg(&self.mount)
                .status();
        }
    }
}

fn place(from: &Path, to: &Path) -> Result<PathBuf> {
    fs::create_dir_all(to.parent().ok_or("a folder")?)?;
    fs::rename(from, to)?;
    Ok(to.canonicalize()?)
}

fn put(bytes: &[u8], to: &Path) -> Result<PathBuf> {
    fs::create_dir_all(to.parent().ok_or("a folder")?)?;
    fs::write(to, bytes)?;
    Ok(to.canonicalize()?)
}

/// Make the photographs, develop them, reorganize them, record them missing and ask the core for
/// its answers. The disk images stay attached until the catalog the editor left has been read.
fn make(generated: &Path) -> Result<(Vec<DiskImage>, Value)> {
    generate_catalog::run(
        &generated.join("source"),
        &generate_catalog::Options {
            seed: SEED,
            files: None,
            assets: None,
            images: Some(IMAGES),
        },
    )?;
    let images = generated.join("source").join("images");
    let manifest = read_json(&images.join("manifest.json"))?;
    let mut pictures = manifest["files"]
        .as_array()
        .ok_or("The image manifest lists no files")?
        .iter()
        .filter_map(|file| file["path"].as_str())
        .map(|path| fs::read(path.split('/').fold(images.clone(), |p, c| p.join(c))));
    let mut next =
        || -> Result<Vec<u8>> { Ok(pictures.next().ok_or("Too few generated images")??) };

    let ssd = DiskImage::create(generated, "Photos SSD")?;
    let mut old = DiskImage::create(generated, "Old SSD")?;
    let ssd_root = ssd.root()?;
    let konstanz = ssd_root.join("2026").join(KONSTANZ);
    let lake = ssd_root.join("2026").join(LAKE);
    let winter = old.root()?.join(WINTER);
    let dump = generated.join("Pictures").join("Card dumps").join(DUMP);
    let mut originals: Vec<(PathBuf, Vec<u8>)> = Vec::new();
    for name in FOUND.iter().chain(&[REWRITTEN, DUPLICATED, DELETED]) {
        let bytes = next()?;
        originals.push((put(&bytes, &konstanz.join(name))?, bytes));
    }
    for name in LAKE_FILES {
        let bytes = next()?;
        originals.push((put(&bytes, &lake.join(name))?, bytes));
    }
    for name in DUMP_FILES {
        let bytes = next()?;
        originals.push((put(&bytes, &dump.join(name))?, bytes));
    }
    for name in WINTER_FILES {
        let bytes = next()?;
        originals.push((put(&bytes, &winter.join(name))?, bytes));
    }
    let other = next()?;

    let catalog = generated.join(generate_catalog::CATALOG);
    drop(EditorService::open(&catalog).map_err(|error| format!("a new catalog: {error}"))?);
    let lake_canonical = lake.canonicalize()?;
    let konstanz_canonical = konstanz.canonicalize()?;
    let assets = with_owner(&catalog, |owner, client| {
        let mut assets = serde_json::Map::new();
        for (index, (path, _)) in originals.iter().enumerate() {
            let started = ask(
                owner,
                client,
                "catalog.import",
                json!({"path": path, "mutation": {"request_id": format!("setup-import-{index}"), "actor": "setup"}}),
            )?;
            let settled = settle(owner, client, &started["job_id"])?;
            ensure(
                settled["status"] == "ready",
                format!("The setup could not develop {}: {settled}", path.display()),
            )?;
            let name = path.file_name().ok_or("a file")?.to_string_lossy();
            assets.insert(name.into_owned(), settled["result"]["asset"]["id"].clone());
        }
        Ok(assets)
    })?;

    // Reorganized within Photos SSD: moved, rewritten, duplicated and deleted.
    let archive = ssd_root.join("Archive");
    let photographs = archive.join("Photographs").join(KONSTANZ);
    let mut moved = serde_json::Map::new();
    for (index, name) in FOUND.iter().enumerate() {
        let now = place(&originals[index].0, &photographs.join(name))?;
        moved.insert((*name).into(), json!(now));
    }
    fs::remove_file(&originals[5].0)?;
    put(&other, &photographs.join(REWRITTEN))?;
    let copies = [
        place(&originals[6].0, &photographs.join(DUPLICATED))?,
        put(&originals[6].1, &archive.join("Backup").join(DUPLICATED))?,
    ];
    let rescued = put(
        &originals[7].1,
        &generated.join("Rescued").join("kept-from-konstanz.JPG"),
    )?;
    fs::remove_file(&originals[7].0)?;
    fs::remove_dir(&konstanz)?;
    // Lake deep in a tree a search is still walking when it is stopped.
    let tree = generated.join("Deep");
    for outer in 0..TREE.0 {
        for inner in 0..TREE.1 {
            fs::create_dir_all(tree.join(format!("{outer:03}")).join(format!("{inner:03}")))?;
        }
    }
    let deepest = tree
        .join(format!("{:03}", TREE.0 - 1))
        .join(format!("{:03}", TREE.1 - 1));
    for (offset, name) in LAKE_FILES.iter().enumerate() {
        fs::copy(&originals[8 + offset].0, deepest.join(name))?;
        fs::remove_file(&originals[8 + offset].0)?;
    }
    fs::remove_dir(&lake)?;
    // The card dump deleted; Old SSD unplugged.
    fs::remove_dir_all(&dump)?;
    old.detach()?;

    let archive = archive.canonicalize()?;
    let tree = tree.canonicalize()?;
    let expected = with_owner(&catalog, |owner, client| {
        let every: Vec<&Value> = assets.values().collect();
        let checked = ask(
            owner,
            client,
            "source.check",
            json!({"targets": {"kind": "assets", "asset_ids": every}}),
        )?;
        let checked = settle(owner, client, &checked["job_id"])?;
        ensure(
            checked["status"] == "ready",
            format!("source.check: {checked}"),
        )?;
        let missing = ask(owner, client, "source.missing", json!({}))?;
        let found = ask(
            owner,
            client,
            "source.find",
            json!({"search_root": archive, "source_folder": konstanz_canonical}),
        )?;
        let found = settle(owner, client, &found["job_id"])?;
        ensure(found["status"] == "ready", format!("source.find: {found}"))?;
        let journal = ask(owner, client, "library.journal", json!({"limit": 500}))?;
        Ok(json!({
            "assets": assets,
            "availability": checked["result"]["rows"],
            "missing": missing,
            "find": found["result"],
            "moved": moved,
            "copies": copies,
            "rescued": rescued,
            "archive": archive,
            "tree": tree,
            "konstanz": konstanz_canonical,
            "lake": lake_canonical,
            "journal": journal["changes"].as_array().map_or(0, Vec::len),
            "startup": ask(owner, client, "volume.list", json!({}))?["volumes"][0]["volume"]["label"],
        }))
    })?;
    Ok((vec![ssd, old], expected))
}

/// What the catalog the editor left says of every photograph, and the changes it recorded.
fn after(generated: &Path, expected: &Value) -> Result<Value> {
    let catalog = generated.join(generate_catalog::CATALOG);
    with_owner(&catalog, |owner, client| {
        let mut locators = serde_json::Map::new();
        for (name, asset) in expected["assets"].as_object().ok_or("no assets")? {
            let state = ask(owner, client, "asset.state", json!({ "asset_id": asset }))?;
            locators.insert(name.clone(), state["asset"]["locator"].clone());
        }
        let journal = ask(owner, client, "library.journal", json!({"limit": 500}))?;
        let changes: Vec<Value> = journal["changes"]
            .as_array()
            .into_iter()
            .flatten()
            .skip(expected["journal"].as_u64().unwrap_or(0) as usize)
            .map(|change| json!({"method": change["method"], "label": change["label"], "actor": change["actor"]}))
            .collect();
        let missing = ask(owner, client, "source.missing", json!({}))?;
        Ok(json!({"locators": locators, "changes": changes, "missing": missing["count"]}))
    })
}

/// Make the catalog and the core's answers (a replay reads the recorded ones), launch the editor
/// over it, read the catalog it left and check the frames.
pub fn run(mut run: Run, scenario: &'static Scenario, sources: Vec<PathBuf>) -> Result {
    let generated = run.out().join(GENERATED);
    let expected_file = run.out().join(EXPECTED);
    let after_file = run.out().join(AFTER);
    if let Some(note) = scenario.note {
        run.note(note);
    }
    run.check(|run| {
        let disks = if run.replaying() {
            None
        } else {
            fs::create_dir_all(&generated)?;
            let (disks, expected) = make(&generated)?;
            write_json(&expected_file, &expected)?;
            Some(disks)
        };
        let expected = read_json(&expected_file)?;
        let text = |key: &str| {
            expected[key]
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("The expected answers name no {key}"))
        };
        let plan = plan(&text("archive")?, &text("tree")?, &text("rescued")?);
        let mut launch = Launch::app()
            .catalog(&generated.join(generate_catalog::CATALOG))
            .script("script.json", plan.script());
        if let Some(window) = scenario.window {
            launch = launch.window(window);
        }
        run.hash(&sources)?;
        let evidence = run.launch(launch)?.dir;
        if let Some(disks) = disks {
            write_json(&after_file, &after(&generated, &expected)?)?;
            drop(disks);
        }
        let checked = plan.check(&evidence)?;
        (scenario.verify)(run, std::slice::from_ref(&checked))?;
        run.sources_unchanged()?;
        Ok(())
    })
}

fn missing(frame: &Frame) -> &Value {
    &frame.state()["missing"]
}

/// A path the core answered, as the frames record it: under the search root, with `/`.
fn under(root: &str, path: &Value) -> Value {
    match path
        .as_str()
        .and_then(|path| Path::new(path).strip_prefix(root).ok())
    {
        Some(rest) => json!(rest.to_string_lossy().replace('\\', "/")),
        None => path.clone(),
    }
}

/// The core's find report as the frames record it.
fn find_record(expected: &Value) -> Value {
    let root = expected["archive"].as_str().unwrap_or_default();
    let rows: Vec<Value> = expected["find"]["rows"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|row| {
            let mut result = json!({"result": row["result"]});
            if row.get("path").is_some() {
                result["path"] = under(root, &row["path"]);
            }
            if let Some(paths) = row["paths"].as_array() {
                result["paths"] = json!(
                    paths
                        .iter()
                        .map(|path| under(root, path))
                        .collect::<Vec<_>>()
                );
            }
            if row.get("by").is_some() {
                result["by"] = row["by"].clone();
            }
            json!({"asset_id": row["asset_id"], "file_name": row["file_name"], "result": result})
        })
        .collect();
    json!(rows)
}

/// A frame's group of `folder`.
fn group<'a>(frame: &'a Frame, folder: &str) -> Result<&'a Value> {
    missing(frame)["groups"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|group| group["folder"] == folder)
        .ok_or_else(|| format!("No group {folder} in {}", missing(frame)).into())
}

/// The rows of a frame's group as the core answered them, without the desktop's choice.
fn rows_answered(group: &Value) -> Value {
    json!(
        group["search"]["rows"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|row| json!({"asset_id": row["asset_id"], "file_name": row["file_name"], "result": row["result"]}))
            .collect::<Vec<_>>()
    )
}

/// The centre draws the groups: the region between the panels holds more than one flat colour.
fn centre_drawn(frame: &Frame) -> Result<Value> {
    let image = frame.image()?;
    let (width, height) = image.dimensions();
    let scale = frame["scale"].as_f64().unwrap_or(1.0);
    let at = |points: f64| (points * scale).round() as u32;
    let (left, right) = (at(241.0) + 8, width.saturating_sub(at(301.0) + 8));
    let (top, bottom) = (at(84.0) + 8, height.saturating_sub(at(90.0)));
    ensure(left < right && top < bottom, "The capture is too small")?;
    let mut colours = std::collections::BTreeMap::<[u8; 3], u32>::new();
    let mut samples = 0u32;
    for y in (top..bottom).step_by(4) {
        for x in (left..right).step_by(4) {
            *colours.entry(image.get_pixel(x, y).0).or_default() += 1;
            samples += 1;
        }
    }
    let most = colours.values().copied().max().unwrap_or(0);
    ensure(
        colours.len() >= 4 && most < samples * 98 / 100,
        format!(
            "Missing originals in {} is blank: {} colours",
            frame.path()?.display(),
            colours.len()
        ),
    )?;
    Ok(json!({"distinct_colours": colours.len(), "most_common_samples": most, "samples": samples}))
}

pub fn verify(run: &mut Run, launches: &[Checked]) -> Result {
    let launch = only(launches)?;
    let expected = read_json(&run.out().join(EXPECTED))?;
    let after = read_json(&run.out().join(AFTER))?;
    let mut checks = Checks::new();
    let asset = |name: &str| expected["assets"][name].clone();

    // Missing originals: the groups are the core's, with their reasons.
    let listed = launch.at("missing")?;
    let block = missing(listed);
    ensure(
        block["shown"] == true && block["quiet"] == true,
        format!("Missing originals was captured unsettled: {block}"),
    )?;
    let answered = &expected["missing"];
    ensure(
        block["count"] == answered["count"],
        format!(
            "The view counts {}, the core {}",
            block["count"], answered["count"]
        ),
    )?;
    let groups: Vec<Value> = answered["groups"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|group| {
            let folder = Path::new(group["source_folder"].as_str().unwrap_or_default())
                .file_name()
                .map(|name| name.to_string_lossy().into_owned());
            json!({"folder": folder, "count": group["count"], "reason": group["reason"]})
        })
        .collect();
    let shown: Vec<Value> = block["groups"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|group| json!({"folder": group["folder"], "count": group["count"], "reason": group["reason"]}))
        .collect();
    ensure(
        shown == groups,
        format!("The view lists {shown:?}, the core {groups:?}"),
    )?;
    ensure(
        groups.len() == 4,
        format!("Four groups are missing, not {}", groups.len()),
    )?;
    let reason = |folder: &str| {
        group(listed, folder).map(|group| {
            group["reason"]["kind"]
                .as_str()
                .unwrap_or_default()
                .to_owned()
        })
    };
    ensure(
        reason(WINTER)? == "volume-offline"
            && reason(KONSTANZ)? == "folder-gone"
            && reason(DUMP)? == "folder-gone",
        "The groups' reasons are not the reorganization's",
    )?;
    let details: Vec<Value> = block["drawn"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|group| group["detail"].clone())
        .collect();
    let startup = expected["startup"].as_str().unwrap_or_default();
    ensure(
        details.iter().any(|detail| {
            detail.as_str().is_some_and(|detail| {
                detail
                    .ends_with("Old SSD is not connected: connect it, or find the files elsewhere")
            })
        }) && details.iter().any(|detail| {
            detail.as_str().is_some_and(|detail| {
                detail.ends_with(&format!("the folder is gone from {startup}"))
            })
        }),
        format!("The groups do not say why they are missing: {details:?}"),
    )?;
    ensure(
        block["title"]
            == format!(
                "{} in the catalog \u{b7} Old SSD is not connected",
                answered["count"]
            ),
        format!("The title bar says {}", block["title"]),
    )?;
    checks.note(
        listed,
        "the groups and their reasons, as source.missing answered",
        json!({"groups": shown, "details": details}),
    );

    // Find in a folder… on Konstanz: every row's result is the core's.
    let found = launch.at("found")?;
    let konstanz = group(found, KONSTANZ)?;
    ensure(
        konstanz["search"]["status"] == "ended",
        format!("The search was captured {}", konstanz["search"]["status"]),
    )?;
    let rows = rows_answered(konstanz);
    let answer = find_record(&expected);
    ensure(
        rows == answer,
        format!("The rows are {rows}, the core answered {answer}"),
    )?;
    let kinds: Vec<&str> = answer
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|row| row["result"]["result"].as_str())
        .collect();
    for kind in ["found", "different-bytes", "several-identical", "not-found"] {
        ensure(
            kinds.contains(&kind),
            format!("No row is {kind}: {kinds:?}"),
        )?;
    }
    let bar = &missing(found)["bar"];
    ensure(
        bar["verified"] == "5" && bar["relink"] == "Relink 5" && bar["stop"] == false,
        format!("The floating bar: {bar}"),
    )?;
    ensure(
        missing(found)["quiet"] == true,
        "The search was captured unsettled",
    )?;
    checks.note(
        found,
        "each photograph's result, as source.find answered",
        json!({"rows": rows, "bar": bar, "centre": centre_drawn(found)?}),
    );

    // The filters.
    let needs = launch.at("needs-you")?;
    let names: Vec<&str> = missing(needs)["drawn"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|group| group["rows"].as_array().into_iter().flatten())
        .filter_map(|row| row["name"].as_str())
        .collect();
    ensure(
        names == [REWRITTEN, DUPLICATED] && missing(needs)["filter"] == "Needs you",
        format!("Needs you shows {names:?}"),
    )?;
    checks.note(needs, "the Needs you filter", json!({"rows": names}));
    let all = launch.at("all")?;
    ensure(missing(all)["filter"] == "All", "All was not chosen again")?;

    // Choose… the second copy: Relink now sends it too.
    let chosen = launch.at("chosen")?;
    // The menu lists the identical files in the order the core answered them.
    let chosen_path = expected["find"]["rows"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|row| row["file_name"] == DUPLICATED)
        .and_then(|row| row["paths"].get(CHOSEN))
        .cloned()
        .ok_or("The core answered no identical files to choose between")?;
    let copies = expected["copies"].as_array().ok_or("no copies")?;
    ensure(
        copies.contains(&chosen_path),
        format!("The core's identical files are not the copies: {chosen_path}"),
    )?;
    let choice = under(
        expected["archive"].as_str().unwrap_or_default(),
        &chosen_path,
    );
    let row = group(chosen, KONSTANZ)?["search"]["rows"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|row| row["file_name"] == DUPLICATED)
        .cloned()
        .ok_or("the duplicated row")?;
    ensure(
        row["chosen"] == choice,
        format!("Choose… left {}, not {choice}", row["chosen"]),
    )?;
    let pairs = missing(chosen)["pairs"]
        .as_array()
        .ok_or("no pairs")?
        .clone();
    ensure(
        pairs.len() == 6
            && pairs
                .iter()
                .any(|pair| pair["asset_id"] == asset(DUPLICATED)),
        format!("Relink would send {pairs:?}"),
    )?;
    checks.note(
        chosen,
        "one of two identical files chosen",
        json!({"chosen": choice, "pairs": pairs}),
    );

    // A row selected: the Info panel describes it.
    let selected = launch.at("row")?;
    let info = &missing(selected)["info"];
    ensure(
        info["kind"] == "one"
            && info["asset_id"] == asset(FOUND[0])
            && info["labels"] == json!(["Was", "Found", "Check", "Folder", "Edits"])
            && info["check"] == "Same size and fingerprint"
            && info["verified"] == true,
        format!("The Info panel: {info}"),
    )?;
    checks.note(
        selected,
        "the Info panel for the selected row",
        info.clone(),
    );

    // Stop search: stopped while it walked, and nothing changed.
    let stopped = launch.at("stopped")?;
    ensure(
        stopped["step"]["stopped_job"].is_string(),
        format!(
            "Stop search was not pressed while the search ran: {}",
            stopped["step"]
        ),
    )?;
    ensure(
        group(stopped, LAKE)?["search"].is_null(),
        "The stopped search's group kept rows",
    )?;
    ensure(
        missing(stopped)["count"] == missing(chosen)["count"],
        "Stopping changed what is missing",
    )?;
    checks.note(
        stopped,
        "a search stopped while it walked, nothing changed",
        json!({"status": stopped.status()?, "job": stopped["step"]["stopped_job"]}),
    );

    // Relink: exactly the verified pairs, one library change.
    let relinked = launch.at("relinked")?;
    ensure(
        relinked["step"]["relink_pairs"] == 6,
        format!("Relink sent {}", relinked["step"]["relink_pairs"]),
    )?;
    let count = answered["count"].as_u64().unwrap_or(0);
    ensure(
        missing(relinked)["count"] == count - 6,
        format!("After Relink {} are missing", missing(relinked)["count"]),
    )?;
    let left: Vec<&str> = group(relinked, KONSTANZ)?["search"]["rows"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|row| row["file_name"].as_str())
        .collect();
    ensure(
        left == [REWRITTEN, DELETED],
        format!("After Relink the rows are {left:?}"),
    )?;
    checks.note(
        relinked,
        "the verified pairs relinked",
        json!({"left": left, "status": relinked.status()?}),
    );

    // Locate… the deleted photograph's copy.
    let located = launch.at("located")?;
    ensure(
        missing(located)["count"] == count - 7,
        format!("After Locate {} are missing", missing(located)["count"]),
    )?;

    // The catalog the editor left: exactly the relinked and the located photographs moved.
    let locators = &after["locators"];
    for name in FOUND {
        ensure(
            locators[name] == expected["moved"][name],
            format!("{name} points at {}, not its found file", locators[name]),
        )?;
    }
    ensure(
        locators[DUPLICATED] == chosen_path,
        format!(
            "{DUPLICATED} points at {}, not the chosen copy",
            locators[DUPLICATED]
        ),
    )?;
    ensure(
        locators[DELETED] == expected["rescued"],
        format!("{DELETED} points at {}", locators[DELETED]),
    )?;
    let konstanz_folder = expected["konstanz"].as_str().unwrap_or_default();
    let lake_folder = expected["lake"].as_str().unwrap_or_default();
    ensure(
        locators[REWRITTEN] == json!(Path::new(konstanz_folder).join(REWRITTEN)),
        format!("{REWRITTEN} moved to {}", locators[REWRITTEN]),
    )?;
    for name in LAKE_FILES {
        ensure(
            locators[name] == json!(Path::new(lake_folder).join(name)),
            format!(
                "{name} moved to {} though its search was stopped",
                locators[name]
            ),
        )?;
    }
    let changes = after["changes"].as_array().ok_or("no changes")?;
    let methods: Vec<&str> = changes
        .iter()
        .filter_map(|change| change["method"].as_str())
        .collect();
    ensure(
        methods == ["source.relink", "source.locate"]
            && changes[0]["label"] == "Relinked 6 originals",
        format!("The editor recorded {changes:?}"),
    )?;
    ensure(
        after["missing"] == count - 7,
        format!("The catalog left {} missing", after["missing"]),
    )?;
    checks.note(
        located,
        "the deleted photograph located at its copy; the catalog the editor left",
        json!({"after": after}),
    );
    checks.write(
        &launch.evidence,
        "resolve-missing",
        json!({"expected": expected}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_resolve_missing_plan_is_well_formed() {
        let plan = plan("/Archive", "/Deep", "/Rescued/kept.JPG");
        plan.validate().unwrap();
        assert_eq!(plan.len(), 11);
        assert!(plan.scripted());
    }
}
