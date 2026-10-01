//! Organizing over `cargo xtask generate-catalog` output, end to end. Ignored by default: each
//! needs a generated directory named by an environment variable, and says `skipped` without one.
//!
//! - `LUXFORGE_GENERATED_IMAGES`: one or more `--images N` outputs (or their `images/` folders),
//!   separated like `PATH`. Every image's header is read through the index's header reader, and
//!   the events (members, place, name), days, cameras and moments (members, kind, evidence,
//!   steps) are compared with `images/manifest.json`, with a probe that answers from the
//!   manifest's steps for the brackets only the previews show (standing in for the preview
//!   lane's), and again with no probe, when those brackets must come out as bursts.
//! - `LUXFORGE_GENERATED_INDEX`: one or more `--files N` outputs (or their `index.sqlite`). The
//!   index's rows are organized and compared with the reference grouping, and every older trip's
//!   folder (`<year>/<first day> <title>`, the plan's ground truth: one trip each) with one event.
use super::*;
use crate::export::metadata::header::read_header;
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

fn generated(variable: &str, inner: &str) -> Option<Vec<PathBuf>> {
    let value = std::env::var_os(variable)?;
    Some(
        std::env::split_paths(&value)
            .map(|dir| {
                if dir.join(inner).exists() {
                    dir.join(inner)
                } else {
                    dir
                }
            })
            .collect(),
    )
}

/// What the manifest says of one file.
struct Truth {
    event: String,
    day: Option<String>,
    body: String,
    moment: String,
    kind: String,
    evidence: Option<String>,
    step: Option<f32>,
}

/// A probe that answers from the manifest: each frame's step (0 when it has none), when every frame
/// is of one moment (the framing is unchanged).
struct ManifestProbe<'a>(&'a HashMap<ViewItem, Truth>);

impl BracketProbe for ManifestProbe<'_> {
    fn measure(&self, frames: &[ViewItem]) -> Option<Vec<f32>> {
        let truths: Vec<&Truth> = frames.iter().map(|item| &self.0[item]).collect();
        if truths.iter().any(|truth| truth.moment != truths[0].moment) {
            return None;
        }
        let first = truths[0].step.unwrap_or(0.0);
        Some(
            truths
                .iter()
                .map(|truth| truth.step.unwrap_or(0.0) - first)
                .collect(),
        )
    }
}

fn text(value: &Value) -> Option<String> {
    value.as_str().map(str::to_owned)
}

/// Reads a generated image folder: its frames through the header reader, and the manifest's truth.
fn read_images(images: &Path) -> (Frames, HashMap<ViewItem, Truth>, Value) {
    let manifest: Value =
        serde_json::from_slice(&std::fs::read(images.join("manifest.json")).unwrap()).unwrap();
    let mut frames = Frames::default();
    let mut truths = HashMap::new();
    for (id, file) in manifest["files"].as_array().unwrap().iter().enumerate() {
        let path = file["path"]
            .as_str()
            .unwrap()
            .split('/')
            .fold(images.to_path_buf(), |path, part| path.join(part));
        let mut handle = std::fs::File::open(&path).unwrap();
        let len = handle.metadata().unwrap().len();
        let header = read_header(&mut handle, len).unwrap().header.metadata();
        let item = ViewItem::File(FileId(id as i64 + 1));
        let capture = header.capture.as_ref();
        assert_eq!(
            capture.map(|capture| capture.instant_ms()),
            file["utc_ms"].as_i64(),
            "{}",
            path.display()
        );
        frames.frames.push(FrameFacts {
            item,
            folder: frames.tables.folder(path.parent().unwrap().to_path_buf()),
            name: path.file_name().unwrap().to_string_lossy().into(),
            instant_ms: capture.map(|capture| capture.instant_ms()),
            local_day: capture.map(|capture| capture.local_day()),
            position: header.position,
            body: frames.tables.body(header.camera.as_ref()),
            exposure: header.exposure,
        });
        truths.insert(
            item,
            Truth {
                event: text(&file["event"]).unwrap(),
                day: text(&file["day"]),
                body: text(&file["body"]).unwrap(),
                moment: text(&file["moment"]).unwrap(),
                kind: text(&file["kind"]).unwrap(),
                evidence: text(&file["bracket_evidence"]),
                step: file["step_ev"].as_f64().map(|step| step as f32),
            },
        );
    }
    (frames, truths, manifest)
}

/// Compares the events of generated images with the manifest's, and answers how many there were.
fn check_events(frames: &Frames, truths: &HashMap<ViewItem, Truth>, manifest: &Value) -> usize {
    let set = frames.events(&Thresholds::default());
    let expected: BTreeMap<&str, &Value> = manifest["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|event| (event["label"].as_str().unwrap(), event))
        .collect();
    let mut seen = BTreeSet::new();
    for (event, members) in set.events.iter().zip(frames.members(&set)) {
        let labels: BTreeSet<&str> = members
            .iter()
            .map(|item| truths[item].event.as_str())
            .collect();
        assert_eq!(labels.len(), 1, "{} mixes events {labels:?}", event.name);
        let label = *labels.first().unwrap();
        assert!(seen.insert(label), "{label} is split: {}", event.name);
        let truth = expected[label];
        assert_eq!(
            members.len() as u64,
            truth["files"].as_u64().unwrap(),
            "{label}"
        );
        assert_eq!(event.place.as_deref(), truth["place"].as_str(), "{label}");
        let days: Vec<LocalDay> = truth["days"]
            .as_array()
            .unwrap()
            .iter()
            .map(|day| serde_json::from_value(day.clone()).unwrap())
            .collect();
        match (truth["place"].as_str(), days.first(), days.last()) {
            (Some(place), Some(first), Some(last)) => {
                assert_eq!(event.name, format!("{place} · {}", dates(*first, *last)));
            }
            (None, None, None) => {
                let folder = truth["folder"].as_str().unwrap_or("Undated");
                assert_eq!(event.name, format!("Undated · {folder}"));
            }
            _ => {}
        }
        println!("  {label}: \"{}\", {} files", event.name, members.len());
    }
    assert_eq!(seen.len(), expected.len(), "every event is found");
    set.events.len()
}

/// Compares the view's days, cameras and moments with the manifest's; `previews` says whether the
/// probe can see the brackets the metadata does not show. Answers the moments found by kind.
fn check_layout(
    frames: &mut Frames,
    truths: &HashMap<ViewItem, Truth>,
    probe: &dyn BracketProbe,
    previews: bool,
) -> BTreeMap<String, usize> {
    let layout = frames.group(&Thresholds::default(), probe);
    let view: Vec<ViewItem> = frames.frames.iter().map(|frame| frame.item).collect();
    let truth = |at: u32| &truths[&view[at as usize]];
    let span = |start: u32, len: u32| start..start + len;
    let days: BTreeSet<Option<&str>> = truths.values().map(|truth| truth.day.as_deref()).collect();
    assert_eq!(layout.days.len(), days.len(), "one group a day");
    for day in &layout.days {
        let found: BTreeSet<_> = span(day.start, day.len)
            .map(|at| truth(at).day.clone())
            .collect();
        assert_eq!(found, BTreeSet::from([day.day.map(|day| day.to_string())]));
        let bodies: BTreeSet<&str> = span(day.start, day.len)
            .map(|at| truth(at).body.as_str())
            .collect();
        let cameras: Vec<_> = layout
            .cameras
            .iter()
            .filter(|camera| span(day.start, day.len).contains(&camera.start))
            .collect();
        if day.day.is_some() && bodies.len() > 1 {
            assert_eq!(cameras.len(), bodies.len(), "{:?}", day.day);
            for camera in cameras {
                assert!(
                    span(camera.start, camera.len).all(|at| truth(at).body == camera.body.0),
                    "{}",
                    camera.label
                );
            }
        } else {
            assert!(cameras.is_empty());
        }
    }
    // Every moment of the manifest's that is not a single is a moment here, exactly.
    let mut expected: BTreeMap<&str, Vec<u32>> = BTreeMap::new();
    for at in 0..view.len() as u32 {
        if truth(at).kind != "single" {
            expected.entry(&truth(at).moment).or_default().push(at);
        }
    }
    let mut counts = BTreeMap::new();
    for moment in &layout.moments {
        let first = truth(moment.start);
        let frames: Vec<u32> = span(moment.start, moment.len).collect();
        assert_eq!(
            expected.remove(first.moment.as_str()).as_ref(),
            Some(&frames),
            "{} ({}) is found as {moment:?}",
            first.moment,
            first.kind
        );
        let metadata_less = first.evidence.as_deref() == Some("previews");
        let (kind, evidence) = match (first.kind.as_str(), metadata_less && !previews) {
            ("bracket", false) => ("bracket", first.evidence.as_deref()),
            _ => ("burst", None),
        };
        let found_kind = match moment.kind {
            MomentKind::Bracket => "bracket",
            MomentKind::Burst => "burst",
            MomentKind::Single => "single",
        };
        let found_evidence = moment.evidence.map(|evidence| match evidence {
            BracketEvidence::Metadata => "metadata",
            BracketEvidence::Previews => "previews",
        });
        assert_eq!(
            (found_kind, found_evidence),
            (kind, evidence),
            "{}",
            first.moment
        );
        if kind == "bracket" {
            let steps: Vec<f32> = frames.iter().map(|at| truth(*at).step.unwrap()).collect();
            assert_steps(moment, &steps);
        }
        *counts
            .entry(
                format!("{kind} {}", evidence.unwrap_or(""))
                    .trim()
                    .to_owned(),
            )
            .or_default() += 1;
    }
    assert!(expected.is_empty(), "moments not found: {expected:?}");
    counts
}

#[test]
#[ignore = "requires generated images: set LUXFORGE_GENERATED_IMAGES"]
fn organize_generated_images_match_their_manifest() {
    let Some(dirs) = generated("LUXFORGE_GENERATED_IMAGES", "images") else {
        println!(
            "skipped: set LUXFORGE_GENERATED_IMAGES to `cargo xtask generate-catalog --images N` \
             outputs"
        );
        return;
    };
    for images in dirs {
        let (mut frames, truths, manifest) = read_images(&images);
        println!("{}: {} files", images.display(), frames.frames.len());
        let events = check_events(&frames, &truths, &manifest);
        let (stays, splits) = check_nights(&frames, &truths);
        let with_probe = check_layout(&mut frames, &truths, &ManifestProbe(&truths), true);
        let without = check_layout(&mut frames, &truths, &NoProbe, false);
        println!(
            "  {events} events; without the night frames {stays} nights stay and {splits} split; \
             moments {with_probe:?}; with no probe {without:?}"
        );
    }
}

/// The generator bridges a trip's nights with a tripod's frames under 3 hours apart, so its trips
/// never need the stay rule. Without those frames (exposures of 10 s or more), a night stays inside
/// its trip exactly when the days either side of it have a positioned frame: every event is then
/// one trip's, and each trip splits at its other nights. Answers the nights that stayed and split.
fn check_nights(frames: &Frames, truths: &HashMap<ViewItem, Truth>) -> (usize, usize) {
    let days = Frames {
        tables: frames.tables.clone(),
        frames: frames
            .frames
            .iter()
            .filter(|frame| frame.exposure.time_s.is_none_or(|time| time < 10.0))
            .cloned()
            .collect(),
    };
    let set = days.events(&Thresholds::default());
    let mut found: BTreeMap<&str, usize> = BTreeMap::new();
    for members in days.members(&set) {
        let labels: BTreeSet<&str> = members
            .iter()
            .map(|item| truths[item].event.as_str())
            .collect();
        assert_eq!(labels.len(), 1, "an event mixes trips: {labels:?}");
        *found.entry(labels.first().unwrap()).or_default() += 1;
    }
    let mut trips: BTreeMap<&str, BTreeMap<&str, bool>> = BTreeMap::new();
    for frame in &days.frames {
        let truth = &truths[&frame.item];
        if let Some(day) = &truth.day {
            *trips
                .entry(&truth.event)
                .or_default()
                .entry(day)
                .or_default() |= frame.position.is_some();
        }
    }
    let (mut stays, mut splits) = (0, 0);
    for (trip, days) in &trips {
        let positioned: Vec<bool> = days.values().copied().collect();
        let split = positioned
            .windows(2)
            .filter(|night| !(night[0] && night[1]))
            .count();
        stays += positioned.len() - 1 - split;
        splits += split;
        assert_eq!(found[trip], 1 + split, "{trip}: {days:?}");
    }
    (stays, splits)
}

/// The index's rows as organizing reads them.
fn read_index(index: &Path) -> (Frames, HashMap<ViewItem, PathBuf>) {
    let connection =
        rusqlite::Connection::open_with_flags(index, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    let mut statement = connection
        .prepare(
            "SELECT id, path, folder, name, capture_ms, local_day, latitude, longitude, make, model,
                 body_serial, exposure_time_s, f_number, iso, exposure_bias_ev, focal_mm,
                 focal_35mm_mm
             FROM files ORDER BY id",
        )
        .unwrap();
    let mut frames = Frames::default();
    let mut paths = HashMap::new();
    let mut rows = statement.query([]).unwrap();
    while let Some(row) = rows.next().unwrap() {
        let day: Option<String> = row.get(5).unwrap();
        let (lat, lon): (Option<f64>, Option<f64>) = (row.get(6).unwrap(), row.get(7).unwrap());
        let (make, model): (Option<String>, Option<String>) =
            (row.get(8).unwrap(), row.get(9).unwrap());
        let camera = make.zip(model).map(|(make, model)| CameraBody {
            make,
            model,
            serial: row.get(10).unwrap(),
        });
        let item = ViewItem::File(FileId(row.get(0).unwrap()));
        paths.insert(item, PathBuf::from(row.get::<_, String>(1).unwrap()));
        frames.frames.push(FrameFacts {
            item,
            folder: frames
                .tables
                .folder(PathBuf::from(row.get::<_, String>(2).unwrap())),
            name: row.get::<_, String>(3).unwrap().into(),
            instant_ms: row.get(4).unwrap(),
            local_day: day.map(|day| serde_json::from_value(Value::String(day)).unwrap()),
            position: lat.zip(lon).map(|(lat, lon)| GeoPosition {
                lat,
                lon,
                alt_m: None,
            }),
            body: frames.tables.body(camera.as_ref()),
            exposure: Exposure {
                time_s: row.get(11).unwrap(),
                f_number: row.get(12).unwrap(),
                iso: row.get(13).unwrap(),
                bias_ev: row.get(14).unwrap(),
                focal_mm: row.get(15).unwrap(),
                focal_35mm_mm: row.get(16).unwrap(),
            },
        });
    }
    (frames, paths)
}

/// An older trip's folder: `…/<year>/<yyyy-mm-dd> <title>`.
fn trip_folder(path: &Path) -> Option<&Path> {
    let folder = path.parent()?;
    let name = folder.file_name()?.to_str()?;
    let year = folder.parent()?.file_name()?.to_str()?;
    (year.len() == 4
        && year.bytes().all(|b| b.is_ascii_digit())
        && name.starts_with(year)
        && name.as_bytes().get(10) == Some(&b' '))
    .then_some(folder)
}

#[test]
#[ignore = "requires a generated index: set LUXFORGE_GENERATED_INDEX"]
fn organize_a_generated_index_matches_the_reference_and_its_trips() {
    let Some(dirs) = generated("LUXFORGE_GENERATED_INDEX", "catalog.index/index.sqlite") else {
        println!(
            "skipped: set LUXFORGE_GENERATED_INDEX to `cargo xtask generate-catalog --files N` \
             outputs"
        );
        return;
    };
    for index in dirs {
        let (frames, paths) = read_index(&index);
        let dated = frames
            .frames
            .iter()
            .filter(|f| f.instant_ms.is_some())
            .count();
        let frames = super::reference::check_against_reference(frames, "the generated index");
        let set = frames.events(&Thresholds::default());
        // Every older trip's folder is exactly one event, holding nothing else. (Its files whose
        // header is pending or unreadable are undated, in the folder's Undated event.)
        let mut trips = 0;
        for (event, members) in set.events.iter().zip(frames.members(&set)) {
            let folders: BTreeSet<Option<&Path>> = members
                .iter()
                .map(|item| trip_folder(&paths[item]))
                .collect();
            if !event.undated() && folders.iter().any(Option::is_some) {
                assert_eq!(folders.len(), 1, "an event mixes trips: {folders:?}");
                trips += 1;
            }
        }
        let trip_folders: BTreeSet<&Path> = frames
            .frames
            .iter()
            .filter(|frame| frame.instant_ms.is_some())
            .filter_map(|frame| trip_folder(&paths[&frame.item]))
            .collect();
        assert_eq!(trips, trip_folders.len(), "every trip is one event");
        let placed = set.events.iter().filter(|e| e.place.is_some()).count();
        println!(
            "{}: {} files ({dated} dated), {} events ({placed} placed, {trips} older trips, {} \
             undated)",
            index.display(),
            frames.frames.len(),
            set.events.len(),
            set.events.iter().filter(|e| e.undated()).count()
        );
        for event in set.events.iter().filter(|event| !event.undated()) {
            println!("  \"{}\", {} files", event.name, event.len);
        }
    }
}
