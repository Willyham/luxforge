//! The `loupe` smoke scenario: the Select workspace's loupe over a folder of real generated JPEGs,
//! in the real editor at 1440 × 900.
//!
//! The run generates a folder of images and a catalog to open (`generate-catalog --images 120
//! --assets 1`, seed 1) into `generated/`, and asks the core for its own answers over a pristine
//! copy before the editor touches it: the folder read by the index lane and viewed as the desktop
//! first views it, and its rows. From them and the images' manifest it chooses a burst of at least
//! three frames and a bracket from metadata. It then reads the folder into the catalog's own index
//! and develops two of its other images into the catalog (`pick.develop`, into the folders its
//! plan proposes), whose originals are real, so a catalog view has photographs to show. The editor then opens the catalog with nothing open.
//! Its frames, in [`plan`] order: `G` showing Select; the folder browsed; the burst's first frame
//! clicked; `E` opening the loupe on it; `→` twice; `1` back to the moment's first frame; `↓` to
//! the next moment and `↑` back; `G` to the grid; the bracket's first frame clicked and `E`;
//! `→`; `Z`, the 100% focus check at the middle of the frame; `C`, the bracket side by side; `P`,
//! picking the bracket's frame where it stands; `Esc` back to the grid; the burst's second
//! frame clicked, `E` and `P`, which picks it and moves on to the next moment's first frame (P7);
//! and `Esc`, All photographs, the first imported photograph clicked (where the core's view of
//! them puts it, beside the generated catalog's own) and `E`, the loupe over it.
//!
//! Each loupe frame's `select.loupe` block is checked against the core's answers: the active frame
//! is the row at the position the step moved to, and the picture drawn is that frame's own (draw
//! identity: its item is the active row's item, in every frame) and its full loupe tier ("Camera
//! preview · 640 × 427", not a stand-in), settled, with the decoded frames and the plan within the
//! budget. What the capture shows is read back: the picture's centre against the frame's own file
//! decoded independently, and on the bracket, nearer its own exposure than its siblings'; the
//! focus check's inset against the same rectangle of the file at 100%. Each `P` is Select's own
//! pick: one `pick.set` recorded as this desktop, the view's pick count one higher, its label in
//! the status bar. Over All photographs the loupe shows an imported photograph's rendered large
//! tier, its middle against its file's, and the strip draws each photograph's own rendered grid
//! tier, which the loupe reads and decodes itself. Everything compared is written to
//! `app/loupe-checks.json`; the core's answers from before the run are kept in
//! `loupe-expected.json`, so a replay checks the same frames against them.
use crate::{
    generate_catalog,
    scenario::{Checked, Checks, Frame, Launch, Plan, Run, Step, Tolerance, plan::only},
    smoke::Scenario,
    *,
};
use luxforge_core::{ApiRequest, ClientId, OwnerHandle};
use luxforge_evidence::{self as script, ArrowKey, SelectStep};

pub const SCENARIO: &str = "loupe";
/// What `reproduce.md` says the run does before it launches.
pub const NOTE: &str = "The run first generates a folder of images and a catalog into \
    `generated/` with `cargo xtask generate-catalog --images 120 --assets 1 --seed 1`, asks the \
    core for the answers the frames are checked against over a pristine copy \
    (`loupe-expected.json`), develops two of the folder's other images into the catalog with \
    `pick.develop`, and launches the editor over that catalog with `--catalog`.";
/// Where the run writes its catalog and images.
pub const GENERATED: &str = "generated";
/// The core's answers from before the run.
pub const EXPECTED: &str = "loupe-expected.json";
const SEED: u64 = 1;
const IMAGES: u32 = 120;
/// What the loupe says a generated JPEG's picture is: the file itself, its own full-size preview.
const SOURCE: &str = "Camera preview \u{b7} 640 \u{d7} 427";
/// What it says a developed photograph's picture is: its rendered large tier, the original's size.
const RENDERED: &str = "Preview \u{b7} 640 \u{d7} 427";
/// The folder's images imported into the catalog as developed photographs.
const PHOTOGRAPHS: usize = 2;
/// How far a patch of the drawn picture may be from the same patch of its file, in 8-bit codes
/// of luminance: the decoders differ in rounding and the picture is resampled to the screen.
const DRAWN_TOLERANCE: f64 = 10.0;

/// Every frame, in order, from the positions the core's answers give.
pub fn plan(expected: &Value) -> Result<Plan> {
    let at = |key: &str| {
        expected[key]["start"]
            .as_u64()
            .map(|start| start as u32)
            .ok_or_else(|| format!("The expected answers name no {key}"))
    };
    let (burst, bracket) = (at("burst")?, at("bracket")?);
    let named = |position: u32| {
        expected["rows"][position as usize]["file_name"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| format!("The expected answers hold no row at {position}"))
    };
    let picked = |position: u32| -> Result<String> {
        Ok(format!("Picked {} \u{b7} Undo \u{2318}Z", named(position)?))
    };
    let folder = expected["folder"]["path"]
        .as_str()
        .ok_or("The expected answers name no folder")?;
    let images = expected["folder"]["count"]
        .as_u64()
        .ok_or("The expected answers hold no folder view")?;
    let photo = photo_position(expected)?;
    let select = |name: &str, step: SelectStep| Step::new(name, script::Step::Select(step));
    let arrow = |name: &str, direction| {
        select(
            name,
            SelectStep::Arrow {
                direction,
                extend: false,
            },
        )
    };
    let click = |name: &str, position| {
        select(
            name,
            SelectStep::Click {
                position,
                shift: false,
                command: false,
            },
        )
    };
    let key = |name: &str, key: &str| Step::new(name, script::Step::key(key));
    Ok(Plan::new(vec![
        Step::opened("opened"),
        key("select", "g"),
        select("folder", SelectStep::Folder(folder.into()))
            .status(format!("images \u{b7} {images} in view")),
        click("burst", burst),
        key("loupe", "e"),
        arrow("frame-2", ArrowKey::Right),
        arrow("frame-3", ArrowKey::Right),
        key("jump-1", "1"),
        arrow("next-moment", ArrowKey::Down),
        arrow("back-moment", ArrowKey::Up),
        key("grid", "g"),
        click("bracket", bracket),
        key("bracket-loupe", "e"),
        arrow("bracket-2", ArrowKey::Right),
        key("focus", "z"),
        key("compare", "c"),
        key("pick", "p").status(picked(bracket + 1)?),
        key("back", script::KEY_ESCAPE),
        click("burst-again", burst + 1),
        key("burst-loupe", "e"),
        key("burst-pick", "p").status(picked(burst + 1)?),
        key("photos-grid", script::KEY_ESCAPE),
        select("photographs", SelectStep::Source("All photographs".into())),
        click("photo", photo),
        key("photo-loupe", "e"),
    ]))
}

/// The position in All photographs, as the core answered it, of the first photograph the run
/// imported: the view also holds the generated catalog's own, whose originals are not on disk.
fn photo_position(expected: &Value) -> Result<u32> {
    let imported = &expected["photographs"]["imported"];
    expected["photographs"]["view"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|row| {
            imported
                .as_array()
                .into_iter()
                .flatten()
                .any(|photo| photo["file_name"] == row["file_name"])
        })
        .and_then(|row| row["position"].as_u64())
        .map(|position| position as u32)
        .ok_or_else(|| "The expected answers show no imported photograph".into())
}

/// One request through the runner's own client.
fn ask(owner: &OwnerHandle, client: ClientId, method: &str, params: Value) -> Result<Value> {
    let response = owner
        .call(
            client,
            ApiRequest {
                id: format!("loupe-smoke-{method}"),
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

fn copy_dir(from: &Path, to: &Path) -> Result {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// The core's answers before the run: the folder of images read and viewed as the desktop first
/// views it, its rows (position, name, path), and the burst and the bracket from metadata the run
/// steps through, chosen from the images' manifest and found in the view.
fn expect(generated: &Path) -> Result<Value> {
    let images = generated.join("images");
    let manifest = read_json(&images.join("manifest.json"))?;
    let files = manifest["files"]
        .as_array()
        .ok_or("The image manifest lists no files")?;
    let scratch =
        std::env::temp_dir().join(format!("luxforge-loupe-expected-{}", std::process::id()));
    let _ = fs::remove_dir_all(&scratch);
    copy_dir(generated, &scratch)?;
    let (owner, join) = OwnerHandle::start(&scratch.join(generate_catalog::CATALOG))
        .map_err(|error| format!("the core cannot open the generated catalog: {error}"))?;
    let client = owner.register();
    let answers = (|| -> Result<Value> {
        let started = ask(
            &owner,
            client,
            "index.refresh",
            json!({"source": {"kind": "folder", "path": images}}),
        )?;
        let job = json!({"job_id": started["job_id"]});
        let read = loop {
            let read = ask(&owner, client, "job.read", job.clone())?;
            if !matches!(read["status"].as_str(), Some("queued" | "running")) {
                break read;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        };
        ensure(
            read["status"] == "ready",
            format!("the core could not read the images: {read}"),
        )?;
        let listed = read["result"]["roots"][0].clone();
        let view = ask(
            &owner,
            client,
            "browse.view",
            json!({"source": {"kind": "folder", "path": listed, "subfolders": true}}),
        )?;
        let count = view["count"]
            .as_u64()
            .ok_or("The folder view has no count")?;
        let rows = ask(
            &owner,
            client,
            "browse.rows",
            json!({"from": 0, "count": count}),
        )?;
        let rows: Vec<Value> = rows["rows"]
            .as_array()
            .ok_or("The folder view answered no rows")?
            .iter()
            .map(|row| {
                json!({
                    "position": row["position"],
                    "file_name": row["file_name"],
                    "path": row["path"],
                })
            })
            .collect();
        // The manifest's kind of the file at a view position, by its path under images/.
        let kind_at = |position: u64| -> Option<(String, Option<String>)> {
            let path = rows.get(position as usize)?["path"].as_str()?;
            let relative = Path::new(path).strip_prefix(&images).ok()?;
            let relative = relative.to_string_lossy().replace('\\', "/");
            let file = files
                .iter()
                .find(|file| file["path"] == relative.as_str())?;
            Some((
                file["kind"].as_str()?.to_owned(),
                file["bracket_evidence"].as_str().map(str::to_owned),
            ))
        };
        let moments = view["groups"]["moments"]
            .as_array()
            .ok_or("The folder view has no moments")?;
        let find = |kind: &str, least: u64, evidence: Option<&str>| {
            moments
                .iter()
                .find(|moment| {
                    let (start, len) = (
                        moment["start"].as_u64().unwrap_or(0),
                        moment["len"].as_u64().unwrap_or(0),
                    );
                    moment["kind"] == kind
                        && len >= least
                        && (start..start + len).all(|at| {
                            kind_at(at).is_some_and(|(truth, found)| {
                                truth == kind
                                    && evidence.is_none_or(|e| found.as_deref() == Some(e))
                            })
                        })
                })
                .cloned()
                .ok_or_else(|| format!("The folder has no {kind} of {least} frames or more"))
        };
        let burst = find("burst", 3, None)?;
        let bracket = find("bracket", 2, Some("metadata"))?;
        Ok(json!({
            "folder": {"path": images, "listed": listed, "count": count},
            "image": manifest["image"],
            "burst": burst,
            "bracket": bracket,
            "rows": rows,
        }))
    })();
    owner.stop();
    let _ = join.join();
    let _ = fs::remove_dir_all(&scratch);
    answers
}

/// A job read until it has ended, within two minutes.
fn settle(owner: &OwnerHandle, client: ClientId, job: &Value) -> Result<Value> {
    let job = json!({"job_id": job});
    let started = std::time::Instant::now();
    loop {
        let read = ask(owner, client, "job.read", job.clone())?;
        if !matches!(read["status"].as_str(), Some("queued" | "running")) {
            return Ok(read);
        }
        ensure(
            started.elapsed() < std::time::Duration::from_secs(120),
            format!("the setup's job did not end: {read}"),
        )?;
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// Develop [`PHOTOGRAPHS`] of the folder's frames into the generated catalog through the core's
/// own `pick.develop` (into the folders its plan proposes): the last of the view outside the
/// burst, the frame after it and the bracket, so the steps over files see nothing change. The
/// folder is read into the catalog's own index first, since a file is developed from its row.
/// Answers their rows (`imported`), and the rows of All photographs as the desktop first views it
/// (`view`): the catalog's photographs newest first, ungrouped, the generated catalog's own among
/// them.
fn develop(generated: &Path, expected: &Value) -> Result<Value> {
    let span = |key: &str| {
        let start = expected[key]["start"].as_u64().unwrap_or(0);
        start..start + expected[key]["len"].as_u64().unwrap_or(0)
    };
    let (burst, bracket) = (span("burst"), span("bracket"));
    let used = |position: u64| {
        (burst.start..=burst.end).contains(&position) || bracket.contains(&position)
    };
    let chosen: Vec<Value> = expected["rows"]
        .as_array()
        .ok_or("The expected answers hold no rows")?
        .iter()
        .rev()
        .filter(|row| row["position"].as_u64().is_some_and(|at| !used(at)))
        .take(PHOTOGRAPHS)
        .cloned()
        .collect();
    ensure(
        chosen.len() == PHOTOGRAPHS,
        "The folder has too few frames to develop",
    )?;
    let (owner, join) = OwnerHandle::start(&generated.join(generate_catalog::CATALOG))
        .map_err(|error| format!("the core cannot open the generated catalog: {error}"))?;
    let client = owner.register();
    let view = (|| -> Result<Vec<Value>> {
        // A file is developed from its row of the catalog's own index, so the folder is read into
        // it first, as the desktop's folder step would.
        let started = ask(
            &owner,
            client,
            "index.refresh",
            json!({"source": {"kind": "folder", "path": generated.join("images")}}),
        )?;
        let read = settle(&owner, client, &started["job_id"])?;
        ensure(
            read["status"] == "ready",
            format!("the core could not read the images: {read}"),
        )?;
        let paths: Vec<Value> = chosen.iter().map(|row| row["path"].clone()).collect();
        let started = ask(
            &owner,
            client,
            "pick.develop",
            json!({
                "targets": {"kind": "paths", "paths": paths},
                "into": [],
                "mutation": {"request_id": "loupe-smoke-develop", "actor": "loupe-smoke"},
            }),
        )?;
        let developed = settle(&owner, client, &started["job_id"])?;
        ensure(
            developed["status"] == "ready",
            format!("the core could not develop the photographs: {developed}"),
        )?;
        let view = ask(
            &owner,
            client,
            "browse.view",
            json!({
                "source": {"kind": "all-photographs"},
                "sort": {"key": "capture-time", "descending": true},
                "grouping": "none",
            }),
        )?;
        let count = view["count"]
            .as_u64()
            .ok_or("All photographs has no count")?;
        let rows = ask(
            &owner,
            client,
            "browse.rows",
            json!({"from": 0, "count": count}),
        )?;
        Ok(rows["rows"]
            .as_array()
            .ok_or("All photographs answered no rows")?
            .iter()
            .map(|row| json!({"position": row["position"], "file_name": row["file_name"]}))
            .collect())
    })();
    owner.stop();
    let _ = join.join();
    Ok(json!({"imported": chosen, "view": view?}))
}

/// Generate the images and the core's answers (a replay reads the recorded ones), plan the launch
/// from them, launch the editor over the catalog and check it.
pub fn run(mut run: Run, scenario: &'static Scenario, sources: Vec<PathBuf>) -> Result {
    let generated = run.out().join(GENERATED);
    let expected_file = run.out().join(EXPECTED);
    if let Some(note) = scenario.note {
        run.note(note);
    }
    run.check(|run| {
        if !run.replaying() {
            generate_catalog::run(
                &generated,
                &generate_catalog::Options {
                    seed: SEED,
                    files: None,
                    assets: Some(1),
                    images: Some(IMAGES),
                },
            )?;
            let mut expected = expect(&generated)?;
            expected["photographs"] = develop(&generated, &expected)?;
            write_json(&expected_file, &expected)?;
        }
        let expected = read_json(&expected_file)?;
        let plan = plan(&expected)?;
        let mut launch = Launch::app()
            .catalog(&generated.join(generate_catalog::CATALOG))
            .script("script.json", plan.script());
        if let Some(window) = scenario.window {
            launch = launch.window(window);
        }
        run.hash(&sources)?;
        let evidence = run.launch(launch)?.dir;
        let checked = plan.check(&evidence)?;
        (scenario.verify)(run, std::slice::from_ref(&checked))?;
        run.sources_unchanged()?;
        run.record("backend", checked.at("loupe")?.state()["backend"].clone());
        Ok(())
    })
}

fn select(frame: &Frame) -> &Value {
    &frame.state()["select"]
}

fn loupe(frame: &Frame) -> &Value {
    &frame.state()["select"]["loupe"]
}

/// The row the core answered at `position`.
fn row(expected: &Value, position: u64) -> Result<&Value> {
    expected["rows"]
        .get(position as usize)
        .ok_or_else(|| format!("The core answered no row at {position}").into())
}

/// Mean luminance of the middle of the image at `path`, a quarter of its short side either way,
/// decoded independently of the editor.
fn file_luminance(path: &str, crop: Option<[u32; 4]>) -> Result<f64> {
    let image = image::open(path)?.to_rgb8();
    let image = match crop {
        Some([x, y, width, height]) => {
            image::imageops::crop_imm(&image, x, y, width, height).to_image()
        }
        None => image,
    };
    let (width, height) = image.dimensions();
    let half = i64::from(width.min(height) / 4);
    crate::scenario::pixels::mean_luminance(
        &image,
        (f64::from(width) / 2.0, f64::from(height) / 2.0),
        half,
    )
}

/// Mean luminance of the middle of `rect` (points, `[x, y, width, height]`) in the capture, a
/// quarter of its short side either way.
fn drawn_luminance(frame: &Frame, rect: &Value) -> Result<f64> {
    let scale = frame["scale"].as_f64().unwrap_or(1.0);
    let at = |index: usize| rect[index].as_f64().unwrap_or(0.0) * scale;
    let (x, y, width, height) = (at(0), at(1), at(2), at(3));
    ensure(
        width > 4.0 && height > 4.0,
        format!("An empty rectangle {rect}"),
    )?;
    crate::scenario::pixels::mean_luminance(
        frame.image()?,
        (x + width / 2.0, y + height / 2.0),
        (width.min(height) / 4.0) as i64,
    )
}

/// One loupe frame: open on the expected position, the picture drawn its own frame's full loupe
/// tier (draw identity), settled, within the budget, and its middle what the frame's file shows.
fn loupe_frame(
    checks: &mut Checks,
    expected: &Value,
    frame: &Frame,
    name: &str,
    position: u64,
) -> Result<f64> {
    let block = loupe(frame);
    ensure(
        block["open"] == true,
        format!("{name}: the loupe is not open"),
    )?;
    ensure(
        block["subject"]["position"] == position,
        format!(
            "{name}: the loupe shows position {}, expected {position}",
            block["subject"]["position"]
        ),
    )?;
    ensure(
        select(frame)["selection"]["active"] == position,
        format!("{name}: the session's active item is not the loupe's"),
    )?;
    let expected_row = row(expected, position)?;
    let active = &block["active"];
    ensure(
        active["name"] == expected_row["file_name"],
        format!(
            "{name}: the loupe names {}, the core's row at {position} is {}",
            active["name"], expected_row["file_name"]
        ),
    )?;
    let picture = &block["picture"];
    ensure(
        block["identity"] == true && picture["item"] == active["item"],
        format!(
            "{name}: the picture drawn is of {}, the frame named is {}",
            picture["item"], active["item"]
        ),
    )?;
    ensure(
        picture["origin"] == "embedded" && picture["stand_in"] == false,
        format!("{name}: the picture is not the frame's full loupe tier: {picture}"),
    )?;
    ensure(
        block["info"]["source"] == SOURCE,
        format!("{name}: the bar says {}", block["info"]["source"]),
    )?;
    let frames = &block["frames"];
    ensure(
        block["settled"] == true && frames["settled"] == true,
        format!("{name}: captured unsettled: {frames}"),
    )?;
    ensure(
        frames["bytes"].as_u64() <= frames["budget"].as_u64()
            && frames["plan_bytes"].as_u64() <= frames["budget"].as_u64(),
        format!("{name}: past the decoded frames' budget: {frames}"),
    )?;
    let path = expected_row["path"]
        .as_str()
        .ok_or("The core's row names no path")?;
    let drawn = drawn_luminance(frame, &active["rect"])?;
    let file = file_luminance(path, None)?;
    checks.compare(
        frame,
        &format!("{name}: the drawn picture's middle against its file's"),
        drawn,
        file,
        Tolerance::Within(DRAWN_TOLERANCE),
    )?;
    checks.note(
        frame,
        &format!("{name}: the loupe on position {position}, its own picture"),
        json!({"active": active, "picture": picture, "info": block["info"], "frames": frames}),
    );
    Ok(drawn)
}

pub fn verify(run: &mut Run, launches: &[Checked]) -> Result {
    let launch = only(launches)?;
    let expected = read_json(&run.out().join(EXPECTED))?;
    let mut checks = Checks::new();
    let span = |key: &str| {
        (
            expected[key]["start"].as_u64().unwrap_or(0),
            expected[key]["len"].as_u64().unwrap_or(0),
        )
    };
    let (burst, burst_len) = span("burst");
    let (bracket, bracket_len) = span("bracket");

    // Draw identity in every frame the loupe is open: the picture is the active frame's own.
    for name in launch.names() {
        let frame = launch.at(name)?;
        let block = loupe(frame);
        if block["open"] == true && !block["picture"].is_null() {
            ensure(
                block["identity"] == true && block["picture"]["item"] == block["active"]["item"],
                format!("{name}: a picture drawn under another frame's name: {block}"),
            )?;
        }
    }

    let folder = launch.at("folder")?;
    ensure(
        select(folder)["count"] == expected["folder"]["count"] && loupe(folder)["open"] == false,
        format!("The folder shows {}", select(folder)["count"]),
    )?;

    // The burst: opened on its first frame, stepped, jumped, and across the moments either side.
    let opened = launch.at("loupe")?;
    loupe_frame(&mut checks, &expected, opened, "loupe", burst)?;
    let block = loupe(opened);
    ensure(
        block["info"]["moment"].as_str().is_some_and(|moment| {
            moment.starts_with("Moment ") && moment.ends_with("\u{b7} burst")
        }) && block["info"]["frame"]
            .as_str()
            .is_some_and(|frame| frame.starts_with(&format!("Frame 1 of {burst_len}"))),
        format!("The bar over the burst: {}", block["info"]),
    )?;
    let strip: Vec<u64> = block["strip"]["positions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_u64)
        .collect();
    ensure(
        strip == (burst..burst + burst_len).collect::<Vec<_>>(),
        format!("The strip holds {strip:?}, the burst is {burst}+{burst_len}"),
    )?;
    for (name, position, number) in [
        ("frame-2", burst + 1, 2),
        ("frame-3", burst + 2, 3),
        ("jump-1", burst, 1),
    ] {
        let frame = launch.at(name)?;
        loupe_frame(&mut checks, &expected, frame, name, position)?;
        ensure(
            loupe(frame)["info"]["frame"]
                .as_str()
                .is_some_and(|text| text.starts_with(&format!("Frame {number} of {burst_len}"))),
            format!("{name}: the bar reads {}", loupe(frame)["info"]["frame"]),
        )?;
    }
    let next = launch.at("next-moment")?;
    loupe_frame(
        &mut checks,
        &expected,
        next,
        "next-moment",
        burst + burst_len,
    )?;
    loupe_frame(
        &mut checks,
        &expected,
        launch.at("back-moment")?,
        "back-moment",
        burst,
    )?;
    let grid = launch.at("grid")?;
    ensure(
        loupe(grid)["open"] == false && select(grid)["selection"]["active"] == burst,
        "G did not return to the grid on the loupe's frame",
    )?;

    // The bracket: each frame's picture nearer its own exposure than its siblings'.
    let exposures: Vec<f64> = (bracket..bracket + bracket_len)
        .map(|position| {
            row(&expected, position)
                .and_then(|row| file_luminance(row["path"].as_str().unwrap_or_default(), None))
        })
        .collect::<Result<_>>()?;
    for (name, position) in [("bracket-loupe", bracket), ("bracket-2", bracket + 1)] {
        let frame = launch.at(name)?;
        let drawn = loupe_frame(&mut checks, &expected, frame, name, position)?;
        ensure(
            loupe(frame)["info"]["moment"]
                .as_str()
                .is_some_and(|moment| moment.ends_with("\u{b7} bracket")),
            format!("{name}: the bar reads {}", loupe(frame)["info"]["moment"]),
        )?;
        let own = exposures[(position - bracket) as usize];
        for (at, other) in exposures.iter().enumerate() {
            if at as u64 + bracket != position && (other - own).abs() > 2.0 * DRAWN_TOLERANCE {
                checks.compare(
                    frame,
                    &format!("{name}: nearer its own exposure than frame {}'s", at + 1),
                    (other - drawn).abs(),
                    (own - drawn).abs(),
                    Tolerance::Above(0.0),
                )?;
            }
        }
    }

    // Z: the region under the middle at 100%, cut from the file's own pixels and labelled so.
    let focus = launch.at("focus")?;
    loupe_frame(&mut checks, &expected, focus, "focus", bracket + 1)?;
    let check = &loupe(focus)["focus_check"];
    let region = &check["region"];
    ensure(
        loupe(focus)["focus"] == true
            && region["origin"] == "embedded"
            && region["item"] == loupe(focus)["active"]["item"]
            && region["rect"] == check["rect"]
            && check["developed"] == false
            && check["pending"] == false,
        format!("The focus check shows {check}"),
    )?;
    let rect = &region["rect"];
    let crop = [
        rect["x"].as_u64().unwrap_or(0) as u32,
        rect["y"].as_u64().unwrap_or(0) as u32,
        rect["width"].as_u64().unwrap_or(0) as u32,
        rect["height"].as_u64().unwrap_or(0) as u32,
    ];
    let inset = &check["inset"];
    // The inset's region is its top, over its 24 pt footer.
    let region_rect = json!([
        inset[0],
        inset[1],
        inset[2],
        inset[3].as_f64().unwrap_or(0.0) - 24.0
    ]);
    let path = row(&expected, bracket + 1)?["path"]
        .as_str()
        .unwrap_or_default();
    checks.compare(
        focus,
        "the inset's region against the same rectangle of the file at 100%",
        drawn_luminance(focus, &region_rect)?,
        file_luminance(path, Some(crop))?,
        Tolerance::Within(DRAWN_TOLERANCE),
    )?;
    checks.note(
        focus,
        "the 100% focus check: the file's own full-size pixels under the pointer",
        json!({"focus_check": check, "regions": loupe(focus)["regions"]}),
    );

    // C: the bracket's frames side by side at one zoom, each its own frame.
    let compare = launch.at("compare")?;
    let cells = loupe(compare)["compare_frames"]
        .as_array()
        .ok_or("Compare recorded no frames")?;
    let shown: Vec<u64> = cells
        .iter()
        .filter_map(|cell| cell["position"].as_u64())
        .collect();
    ensure(
        loupe(compare)["compare"] == true
            && shown == (bracket..bracket + bracket_len.min(4)).collect::<Vec<_>>(),
        format!("Compare shows {shown:?}"),
    )?;
    for cell in cells {
        ensure(
            cell["picture"]["item"] == cell["item"],
            format!("A compare cell draws another frame's picture: {cell}"),
        )?;
        let position = cell["position"].as_u64().unwrap_or(0);
        let path = row(&expected, position)?["path"]
            .as_str()
            .unwrap_or_default();
        checks.compare(
            compare,
            &format!(
                "compare: frame {}'s middle against its file's",
                cell["number"]
            ),
            drawn_luminance(compare, &cell["rect"])?,
            file_luminance(path, None)?,
            Tolerance::Within(DRAWN_TOLERANCE),
        )?;
    }
    let widths: Vec<&Value> = cells.iter().map(|cell| &cell["rect"][2]).collect();
    ensure(
        widths.windows(2).all(|pair| pair[0] == pair[1]),
        format!("Compare's frames are at different zooms: {widths:?}"),
    )?;

    // P on the bracket's frame: Select's own pick of it, and the loupe stays where it is.
    let pick = launch.at("pick")?;
    pick_frame(&mut checks, &expected, compare, pick, "pick", bracket + 1)?;
    ensure(
        loupe(pick)["open"] == true
            && loupe(pick)["compare"] == true
            && loupe(pick)["subject"]["position"] == bracket + 1
            && select(pick)["selection"]["active"] == bracket + 1,
        format!(
            "pick: a bracket's pick moved the loupe or left compare: {}",
            loupe(pick)["subject"]
        ),
    )?;

    let back = launch.at("back")?;
    ensure(
        loupe(back)["open"] == false && select(back)["selection"]["active"] == bracket + 1,
        "Esc did not return to the grid on the loupe's frame",
    )?;

    // P on a burst's frame picks it and moves on to the next moment's first frame (P7).
    loupe_frame(
        &mut checks,
        &expected,
        launch.at("burst-loupe")?,
        "burst-loupe",
        burst + 1,
    )?;
    let burst_pick = launch.at("burst-pick")?;
    pick_frame(
        &mut checks,
        &expected,
        launch.at("burst-loupe")?,
        burst_pick,
        "burst-pick",
        burst + 1,
    )?;
    loupe_frame(
        &mut checks,
        &expected,
        burst_pick,
        "burst-pick",
        burst + burst_len,
    )?;
    photo_loupe(&mut checks, &expected, launch.at("photo-loupe")?)?;
    checks.write(
        &launch.evidence,
        "loupe",
        json!({"expected": {"burst": expected["burst"], "bracket": expected["bracket"],
            "folder": expected["folder"], "photographs": expected["photographs"]},
            "tolerance": DRAWN_TOLERANCE}),
    )
}

/// `E` over All photographs on the imported photograph clicked: the loupe on it, its picture its
/// own rendered large tier, settled, its middle what its original shows; and the strip, each of
/// whose frames is a photograph drawn from its own rendered grid tier, which the loupe read and
/// decoded itself.
fn photo_loupe(checks: &mut Checks, expected: &Value, frame: &Frame) -> Result {
    let block = loupe(frame);
    let active = &block["active"];
    let position = photo_position(expected)?;
    let original = expected["photographs"]["imported"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|row| row["file_name"] == active["name"])
        .ok_or_else(|| {
            format!(
                "photo-loupe: the loupe shows {}, not an imported photograph",
                active["name"]
            )
        })?;
    ensure(
        block["open"] == true
            && active["item"]["kind"] == "photo"
            && active["position"] == position
            && select(frame)["selection"]["active"] == position,
        format!("photo-loupe: the loupe is not on the photograph at {position}: {active}"),
    )?;
    let picture = &block["picture"];
    ensure(
        block["identity"] == true
            && picture["item"] == active["item"]
            && picture["origin"] == "rendered"
            && picture["stand_in"] == false
            && block["info"]["source"] == RENDERED,
        format!(
            "photo-loupe: the picture is not the photograph's own rendered tier: {picture}, {}",
            block["info"]["source"]
        ),
    )?;
    let frames = &block["frames"];
    ensure(
        block["settled"] == true
            && frames["settled"] == true
            && frames["bytes"].as_u64() <= frames["budget"].as_u64(),
        format!("photo-loupe: captured unsettled or past the budget: {frames}"),
    )?;
    let thumbnails = block["strip"]["thumbnails"]
        .as_array()
        .filter(|thumbnails| !thumbnails.is_empty())
        .ok_or("photo-loupe: the strip recorded no frames")?;
    for thumbnail in thumbnails {
        ensure(
            thumbnail["item"]["kind"] == "photo"
                && thumbnail["drawn"] == true
                && thumbnail["picture"]["item"] == thumbnail["item"]
                && thumbnail["picture"]["origin"] == "rendered"
                && thumbnail["picture"]["stand_in"] == false,
            format!("photo-loupe: a strip frame is not its photograph's grid tier: {thumbnail}"),
        )?;
    }
    ensure(
        frames["thumbnails"].as_u64() == Some(thumbnails.len() as u64)
            && frames["thumbnails_held"] == frames["thumbnails"],
        format!("photo-loupe: the strip's thumbnails are not all held: {frames}"),
    )?;
    checks.compare(
        frame,
        "photo-loupe: the drawn photograph's middle against its original's",
        drawn_luminance(frame, &active["rect"])?,
        file_luminance(original["path"].as_str().unwrap_or_default(), None)?,
        Tolerance::Within(DRAWN_TOLERANCE),
    )?;
    checks.note(
        frame,
        "photo-loupe: a developed photograph in the loupe, the strip its own grid tiers",
        json!({"active": active, "picture": picture, "info": block["info"],
            "strip": block["strip"], "frames": frames}),
    );
    Ok(())
}

/// A `P` in the loupe: the last library request is one `pick.set` of the file at `position`, as
/// this desktop, recorded as a change, and the view's pick count is one higher than `before`'s.
fn pick_frame(
    checks: &mut Checks,
    expected: &Value,
    before: &Frame,
    frame: &Frame,
    name: &str,
    position: u64,
) -> Result {
    let library = &select(frame)["library"];
    ensure(
        library["method"] == "pick.set"
            && library["params"]["picked"] == true
            && library["error"].is_null()
            && !library["answer"]["change"].is_null(),
        format!("{name}: the pick sent {library}"),
    )?;
    let files = library["params"]["targets"]["file_ids"]
        .as_array()
        .map_or(0, Vec::len);
    ensure(
        files == 1,
        format!("{name}: the pick named {files} files: {library}"),
    )?;
    let count = |frame: &Frame| select(frame)["picked"].as_u64().unwrap_or(0);
    ensure(
        count(frame) == count(before) + 1,
        format!(
            "{name}: the view holds {} picks, {} before",
            count(frame),
            count(before)
        ),
    )?;
    checks.note(
        frame,
        &format!(
            "{name}: P picked {} (position {position})",
            row(expected, position)?["file_name"]
        ),
        json!({"library": library, "picked": count(frame), "status": frame.status()?}),
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_loupe_plan_is_well_formed() {
        let expected = json!({
            "folder": {"path": "/generated/images", "count": 120},
            "burst": {"start": 3, "len": 4},
            "bracket": {"start": 20, "len": 3},
            "rows": (0..120)
                .map(|at| json!({"position": at, "file_name": format!("IMG_{at:04}.JPG")}))
                .collect::<Vec<_>>(),
            "photographs": {
                "imported": [{"position": 119, "file_name": "IMG_0119.JPG"}],
                "view": [
                    {"position": 0, "file_name": "L1003201.DNG"},
                    {"position": 1, "file_name": "IMG_0119.JPG"},
                ],
            },
        });
        assert_eq!(photo_position(&expected).unwrap(), 1, "the imported one");
        let plan = plan(&expected).unwrap();
        plan.validate().unwrap();
        assert_eq!(plan.len(), 25);
        assert!(plan.scripted());
    }
}
