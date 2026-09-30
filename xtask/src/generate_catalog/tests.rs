use super::plan::{BracketKind, Event, Frame, MomentKind, Plan};
use super::*;
use exif::{In, Tag, Value as ExifValue};
use image::GenericImageView;
use luxforge_testbase::paths::temp_dir;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Over the September trips' minimum, so every kind of moment is there.
const COUNT: u32 = 110;

/// A generated image folder in a scratch directory, removed when dropped.
struct Generated(PathBuf);

impl Generated {
    fn new(seed: u64, count: u32, label: &str) -> Self {
        let root = temp_dir(label);
        run(
            &root.join("out"),
            &Options {
                seed,
                files: None,
                assets: None,
                images: Some(count),
            },
        )
        .unwrap();
        Generated(root)
    }

    fn images(&self) -> PathBuf {
        self.0.join("out/images")
    }

    fn manifest(&self) -> Value {
        crate::read_json(&self.images().join("manifest.json")).unwrap()
    }

    fn entries(&self) -> Vec<Value> {
        self.manifest()["files"].as_array().unwrap().clone()
    }

    fn read(&self, entry: &Value) -> Vec<u8> {
        let path = entry["path"].as_str().unwrap();
        fs::read(path.split('/').fold(self.images(), |p, c| p.join(c))).unwrap()
    }

    /// Every file under `images/` with its bytes, by relative path.
    fn listing(&self) -> BTreeMap<PathBuf, Vec<u8>> {
        crate::files(&self.images())
            .unwrap()
            .into_iter()
            .map(|path| {
                let bytes = fs::read(&path).unwrap();
                (path.strip_prefix(self.images()).unwrap().to_owned(), bytes)
            })
            .collect()
    }
}

impl Drop for Generated {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn the_same_seed_writes_the_same_bytes_and_another_seed_does_not() {
    let first = Generated::new(1, COUNT, "generate-catalog-first");
    let again = Generated::new(1, COUNT, "generate-catalog-again");
    let other = Generated::new(2, COUNT, "generate-catalog-other");
    let listing = first.listing();
    assert_eq!(
        listing.len(),
        COUNT as usize + 1,
        "the images and the manifest"
    );
    assert!(
        listing == again.listing(),
        "the same seed wrote different bytes"
    );
    let other_listing = other.listing();
    assert_ne!(first.manifest(), other.manifest());
    let same = listing
        .iter()
        .filter(|(path, bytes)| other_listing.get(*path) == Some(bytes))
        .count();
    assert_eq!(same, 0, "another seed wrote {same} identical files");
}

#[test]
fn a_count_below_the_plans_minimum_or_an_existing_output_is_refused() {
    let root = temp_dir("generate-catalog-refusals");
    let options = |images| Options {
        seed: 1,
        files: None,
        assets: None,
        images: Some(images),
    };
    let error = run(&root.join("small"), &options(10)).unwrap_err();
    assert!(error.to_string().contains("too few"), "{error}");
    assert!(
        !root.join("small").exists(),
        "a refused count wrote nothing"
    );
    let error = run(&root, &options(COUNT)).unwrap_err();
    assert!(error.to_string().contains("must be new"), "{error}");
    fs::remove_dir_all(&root).unwrap();
}

fn text(exif: &exif::Exif, tag: Tag) -> Option<String> {
    match &exif.get_field(tag, In::PRIMARY)?.value {
        ExifValue::Ascii(parts) => Some(String::from_utf8(parts[0].clone()).unwrap()),
        other => panic!("{tag} is {other:?}"),
    }
}

fn number(exif: &exif::Exif, tag: Tag, ifd: In) -> u32 {
    exif.get_field(tag, ifd)
        .unwrap_or_else(|| panic!("{tag} is missing"))
        .value
        .get_uint(0)
        .unwrap()
}

fn rationals(exif: &exif::Exif, tag: Tag) -> Vec<(u32, u32)> {
    match &exif.get_field(tag, In::PRIMARY).unwrap().value {
        ExifValue::Rational(values) => values.iter().map(|r| (r.num, r.denom)).collect(),
        other => panic!("{tag} is {other:?}"),
    }
}

fn degrees(exif: &exif::Exif, tag: Tag, reference: Tag, negative: &str) -> f64 {
    let parts: Vec<f64> = rationals(exif, tag)
        .into_iter()
        .map(|(num, den)| f64::from(num) / f64::from(den))
        .collect();
    let value = parts[0] + parts[1] / 60.0 + parts[2] / 3600.0;
    if text(exif, reference).unwrap() == negative {
        -value
    } else {
        value
    }
}

fn close(a: f64, b: &Value) -> bool {
    (a - b.as_f64().unwrap()).abs() < 1e-9
}

/// The EXIF of a written file and its decoded thumbnail.
fn exif_of(bytes: &[u8]) -> (exif::Exif, image::DynamicImage) {
    // SOI, the JFIF APP0, then one APP1 holding all of it.
    assert_eq!(&bytes[..4], &[0xff, 0xd8, 0xff, 0xe0]);
    let app1 = 4 + usize::from(u16::from_be_bytes([bytes[4], bytes[5]]));
    assert_eq!(&bytes[app1..app1 + 2], &[0xff, 0xe1], "APP1 follows APP0");
    assert_eq!(&bytes[app1 + 4..app1 + 10], b"Exif\0\0");
    let exif = exif::Reader::new()
        .read_from_container(&mut std::io::Cursor::new(bytes))
        .unwrap();
    let offset = number(&exif, Tag::JPEGInterchangeFormat, In::THUMBNAIL) as usize;
    let length = number(&exif, Tag::JPEGInterchangeFormatLength, In::THUMBNAIL) as usize;
    let thumbnail = image::load_from_memory(&exif.buf()[offset..offset + length]).unwrap();
    (exif, thumbnail)
}

#[test]
fn every_field_written_reads_back_with_a_thumbnail_of_the_stated_size() {
    let generated = Generated::new(3, COUNT, "generate-catalog-fields");
    let manifest = generated.manifest();
    let size = |key: &str| {
        (
            manifest[key]["width"].as_u64().unwrap() as u32,
            manifest[key]["height"].as_u64().unwrap() as u32,
        )
    };
    for entry in generated.entries() {
        let bytes = generated.read(&entry);
        let (exif, thumbnail) = exif_of(&bytes);
        let path = &entry["path"];
        assert_eq!(thumbnail.dimensions(), size("thumbnail"), "{path}");
        assert_eq!(
            image::load_from_memory(&bytes).unwrap().dimensions(),
            size("image"),
            "{path}"
        );
        let body: Vec<&str> = entry["body"].as_str().unwrap().split('|').collect();
        assert_eq!(text(&exif, Tag::Make).as_deref(), Some(body[0]), "{path}");
        assert_eq!(text(&exif, Tag::Model).as_deref(), Some(body[1]), "{path}");
        assert_eq!(
            text(&exif, Tag::BodySerialNumber).unwrap_or_default(),
            body[2],
            "{path}"
        );
        assert!(text(&exif, Tag::LensModel).is_some(), "{path}");
        assert_eq!(number(&exif, Tag::Orientation, In::PRIMARY), 1);
        assert_eq!(
            [Tag::PixelXDimension, Tag::PixelYDimension].map(|tag| number(&exif, tag, In::PRIMARY)),
            [size("image").0, size("image").1],
        );
        match entry["capture"].as_str() {
            None => {
                for tag in [
                    Tag::DateTimeOriginal,
                    Tag::SubSecTimeOriginal,
                    Tag::OffsetTimeOriginal,
                ] {
                    assert!(text(&exif, tag).is_none(), "{path} is undated");
                }
            }
            Some(capture) => {
                // `2026-09-12T10:15:02.130+02:00`, the offset only when written.
                let (date, rest) = capture.split_once('T').unwrap();
                assert_eq!(
                    text(&exif, Tag::DateTimeOriginal).unwrap(),
                    format!("{} {}", date.replace('-', ":"), &rest[..8]),
                );
                assert_eq!(text(&exif, Tag::SubSecTimeOriginal).unwrap(), &rest[9..12]);
                let offset = (rest.len() > 12).then(|| rest[12..].to_owned());
                assert_eq!(text(&exif, Tag::OffsetTimeOriginal), offset, "{path}");
            }
        }
        let exposure = &entry["exposure"];
        let (num, den) = rationals(&exif, Tag::ExposureTime)[0];
        assert_eq!(format!("{num}/{den}"), exposure["exposure_time"]);
        let (num, den) = rationals(&exif, Tag::FNumber)[0];
        assert!(close(
            f64::from(num) / f64::from(den),
            &exposure["f_number"]
        ));
        assert_eq!(
            number(&exif, Tag::PhotographicSensitivity, In::PRIMARY),
            exposure["iso"].as_u64().unwrap() as u32
        );
        let bias = match &exif
            .get_field(Tag::ExposureBiasValue, In::PRIMARY)
            .unwrap()
            .value
        {
            ExifValue::SRational(values) => f64::from(values[0].num) / f64::from(values[0].denom),
            other => panic!("bias is {other:?}"),
        };
        assert!(close(bias, &exposure["bias_ev"]), "{path}");
        let (num, den) = rationals(&exif, Tag::FocalLength)[0];
        assert!(close(
            f64::from(num) / f64::from(den),
            &exposure["focal_length_mm"]
        ));
        assert_eq!(
            number(&exif, Tag::FocalLengthIn35mmFilm, In::PRIMARY),
            exposure["focal_length_35mm"].as_u64().unwrap() as u32
        );
        match entry["gps"].as_object() {
            None => assert!(exif.get_field(Tag::GPSLatitude, In::PRIMARY).is_none()),
            Some(gps) => {
                let latitude = degrees(&exif, Tag::GPSLatitude, Tag::GPSLatitudeRef, "S");
                let longitude = degrees(&exif, Tag::GPSLongitude, Tag::GPSLongitudeRef, "W");
                assert!(close(latitude, &gps["latitude"]), "{path}");
                assert!(close(longitude, &gps["longitude"]), "{path}");
                let (num, den) = rationals(&exif, Tag::GPSAltitude)[0];
                assert!(close(f64::from(num) / f64::from(den), &gps["altitude_m"]));
                assert_eq!(number(&exif, Tag::GPSAltitudeRef, In::PRIMARY), 0);
                assert!(exif.get_field(Tag::GPSVersionID, In::PRIMARY).is_some());
            }
        }
    }
}

/// The mean log2 luminance of a thumbnail, in linear light.
fn mean_log_luminance(thumbnail: &image::DynamicImage) -> f64 {
    let rgb = thumbnail.to_rgb8();
    let sum: f64 = rgb
        .pixels()
        .map(|pixel| {
            let [r, g, b] = pixel.0.map(luxforge_reference::srgb::decode);
            (0.2126 * r + 0.7152 * g + 0.0722 * b).max(1e-4).log2()
        })
        .sum();
    sum / f64::from(rgb.width() * rgb.height())
}

#[test]
fn a_brackets_thumbnails_step_by_its_written_exposure_steps() {
    let generated = Generated::new(4, COUNT, "generate-catalog-brackets");
    let mut brackets: BTreeMap<String, Vec<(f64, f64)>> = BTreeMap::new();
    for entry in generated.entries() {
        if entry["kind"] != "bracket" {
            continue;
        }
        let (_, thumbnail) = exif_of(&generated.read(&entry));
        brackets
            .entry(entry["moment"].as_str().unwrap().to_owned())
            .or_default()
            .push((
                entry["step_ev"].as_f64().unwrap(),
                mean_log_luminance(&thumbnail),
            ));
    }
    assert!(brackets.len() >= 6, "{} brackets", brackets.len());
    for (moment, frames) in brackets {
        let (base_step, base_mean) = frames[0];
        for &(step, mean) in &frames[1..] {
            let measured = mean - base_mean;
            assert!(
                (measured - (step - base_step)).abs() < 0.05,
                "{moment}: {step} EV against {base_step} EV measured {measured:.3} EV"
            );
        }
    }
}

#[test]
fn the_layout_holds_a_card_dumps_and_user_named_folders() {
    let generated = Generated::new(5, COUNT, "generate-catalog-layout");
    let entries = generated.entries();
    let paths: Vec<&str> = entries
        .iter()
        .map(|e| e["path"].as_str().unwrap())
        .collect();
    for path in [
        "card/DCIM/100NZ8_1/DSC_0001.JPG",
        "card/DCIM/100NZ8_2/DSC_0001.JPG",
        "Card dumps/2026-09-12/L1003201.JPG",
        "Card dumps/2026-09-18/DJI_0001.JPG",
        "2026-09-14 Lake/DSCF0001.JPG",
        "iPhone export/IMG_4201.JPG",
    ] {
        assert!(paths.contains(&path), "{path} is missing");
    }
    let undated: Vec<&Value> = entries.iter().filter(|e| e["capture"].is_null()).collect();
    assert!(undated.len() >= 3);
    assert!(
        undated
            .iter()
            .all(|e| e["path"].as_str().unwrap().starts_with("From Anna/")
                && e["event"] == "undated"
                && e["day"].is_null())
    );
    let mut listed: Vec<PathBuf> = paths
        .iter()
        .map(|p| p.split('/').collect::<PathBuf>())
        .collect();
    listed.push("manifest.json".into());
    listed.sort();
    let written: Vec<PathBuf> = generated.listing().into_keys().collect();
    assert_eq!(written, listed, "the manifest lists every file written");
}

fn distance_km(a: &Frame, b: &Frame) -> f64 {
    let (a, b) = (a.gps.unwrap(), b.gps.unwrap());
    let radians = |micro: i32| f64::from(micro) / 1e6 * std::f64::consts::PI / 180.0;
    let (lat1, lat2) = (radians(a.latitude), radians(b.latitude));
    let dlat = lat2 - lat1;
    let dlon = radians(b.longitude) - radians(a.longitude);
    let h = (dlat / 2.0).sin().powi(2) + lat1.cos() * lat2.cos() * (dlon / 2.0).sin().powi(2);
    2.0 * 6371.0 * h.sqrt().asin()
}

const THREE_HOURS: i64 = 3 * 3600 * 1000;

/// What a whole plan holds, once [`check_events`] has checked it.
#[derive(Default)]
struct Found {
    kinds: Vec<MomentKind>,
    jumps: usize,
    unpositioned: usize,
    undated: usize,
}

/// Every property the plan promises the event and moment rules, over a whole plan in time order:
/// gaps under 3 hours and positioned neighbours within 25 km inside an event; over 3 hours, or a
/// jump of over 25 km inside an hour, between events; one body's moments over 2 s apart with its
/// file numbers in time order; bursts and brackets as P5 reads them; moments in capture order; and
/// every recorded time's instant, as the core reads it, the frame's true instant.
fn check_events(events: &[Event]) -> Found {
    let mut found = Found::default();
    let mut previous: Option<Frame> = None;
    for event in events {
        if event.place.is_none() {
            assert!(event.frames.iter().all(|f| f.time.is_none()));
            found.undated += event.frames.len();
            continue;
        }
        for frame in &event.frames {
            let header = super::disk::header(frame, false, 0).unwrap();
            assert_eq!(
                header.capture.map(|time| time.instant_ms()),
                frame.utc(),
                "{}: the core reads another instant",
                event.label
            );
        }
        found.unpositioned += usize::from(!event.positioned());
        let mut frames = event.frames.clone();
        frames.sort_by_key(|f| f.utc().unwrap());
        let (first, last) = (frames[0], *frames.last().unwrap());
        if let Some(before) = previous {
            let gap = first.utc().unwrap() - before.utc().unwrap();
            assert!(
                gap > 0,
                "{} starts before the event before ends",
                event.label
            );
            if gap <= THREE_HOURS {
                assert!(
                    gap < 3600 * 1000,
                    "{}: a close event is under an hour on",
                    event.label
                );
                assert!(
                    before.gps.is_some()
                        && first.gps.is_some()
                        && distance_km(&before, &first) > 25.0,
                    "{} is under 3 hours from the event before without a 25 km jump",
                    event.label
                );
                assert_eq!(before.time.unwrap().date(), first.time.unwrap().date());
                found.jumps += 1;
            }
        }
        previous = Some(last);
        for pair in frames.windows(2) {
            let gap = pair[1].utc().unwrap() - pair[0].utc().unwrap();
            assert!(gap < THREE_HOURS, "{}: a {gap} ms gap", event.label);
            if pair[0].gps.is_some() && pair[1].gps.is_some() {
                assert!(distance_km(&pair[0], &pair[1]) < 25.0, "{}", event.label);
            }
        }
        let mut by_body: BTreeMap<usize, Vec<Frame>> = BTreeMap::new();
        for frame in &frames {
            by_body.entry(frame.body).or_default().push(*frame);
        }
        for body_frames in by_body.values() {
            for pair in body_frames.windows(2) {
                let gap = pair[1].utc().unwrap() - pair[0].utc().unwrap();
                if pair[0].moment != pair[1].moment {
                    assert!(gap > 2000, "{}: moments {gap} ms apart", event.label);
                }
            }
            let counts: Vec<u32> = body_frames.iter().map(|f| f.count).collect();
            assert!(
                counts.windows(2).all(|c| c[0] < c[1]),
                "file numbers follow time"
            );
        }
        let mut moments: BTreeMap<u32, Vec<Frame>> = BTreeMap::new();
        for frame in &event.frames {
            moments.entry(frame.moment).or_default().push(*frame);
        }
        let starts: Vec<i64> = moments.values().map(|m| m[0].utc().unwrap()).collect();
        assert!(
            starts.windows(2).all(|s| s[0] <= s[1]),
            "{}: moments in capture order, so no day ran into the next",
            event.label
        );
        for frames in moments.values() {
            let kind = frames[0].kind;
            found.kinds.push(kind);
            assert!(
                frames
                    .iter()
                    .all(|f| f.kind == kind && f.body == frames[0].body)
            );
            let gaps: Vec<i64> = frames
                .windows(2)
                .map(|p| p[1].utc().unwrap() - p[0].utc().unwrap())
                .collect();
            let same = |key: fn(&Frame) -> u32| frames.iter().all(|f| key(f) == key(&frames[0]));
            match kind {
                MomentKind::Single => assert_eq!(frames.len(), 1),
                MomentKind::Burst => {
                    assert!((3..=8).contains(&frames.len()));
                    assert!(gaps.iter().all(|&g| (0..1000).contains(&g)));
                    assert!(frames.iter().all(|f| f.exposure == frames[0].exposure));
                }
                MomentKind::Bracket(bracket) => {
                    assert!([3, 5].contains(&frames.len()));
                    assert!(gaps.iter().all(|&g| (300..=1800).contains(&g)));
                    assert!(same(|f| f.exposure.f_number));
                    assert!(same(|f| u32::from(f.exposure.iso)));
                    assert!(same(|f| f.exposure.focal));
                    let steps: Vec<i8> = frames.iter().map(|f| f.step.unwrap()).collect();
                    assert!(steps.iter().all(|s| (-2..=2).contains(s)));
                    let times_differ = frames
                        .iter()
                        .any(|f| f.exposure.time != frames[0].exposure.time);
                    let biases: Vec<i32> = frames.iter().map(|f| f.exposure.bias).collect();
                    match bracket {
                        BracketKind::WithBias => {
                            assert!(times_differ);
                            let expected: Vec<i32> =
                                steps.iter().map(|&s| i32::from(s) * 3).collect();
                            assert_eq!(biases, expected);
                        }
                        BracketKind::WithoutBias => {
                            assert!(times_differ);
                            assert!(biases.iter().all(|&b| b == 0));
                        }
                        BracketKind::MetadataLess => {
                            assert!(frames.iter().all(|f| f.exposure == frames[0].exposure));
                        }
                    }
                }
            }
        }
    }
    found
}

const EVERY_KIND: [MomentKind; 5] = [
    MomentKind::Single,
    MomentKind::Burst,
    MomentKind::Bracket(BracketKind::WithBias),
    MomentKind::Bracket(BracketKind::WithoutBias),
    MomentKind::Bracket(BracketKind::MetadataLess),
];

#[test]
fn every_plan_keeps_the_event_and_moment_rules_and_its_count() {
    let count = |events: &[Event]| events.iter().map(|e| e.frames.len() as u32).sum::<u32>();
    for seed in 1..=6 {
        for total in [
            super::plan::september().iter().map(|s| s.minimum()).sum(),
            150,
            700,
            super::plan::MOST_IMAGES,
        ] {
            let events: Vec<Event> = Plan::images(seed, total).unwrap().collect();
            assert_eq!(count(&events), total);
            let days: Vec<usize> = events.iter().map(|e| e.days().len()).collect();
            assert_eq!(days, [2, 1, 1, 1, 3, 0], "seed {seed}, {total} frames");
            let found = check_events(&events);
            assert_eq!(found.jumps, 1, "one same-day jump of over 25 km");
            assert!(EVERY_KIND.iter().all(|kind| found.kinds.contains(kind)));
            assert_eq!(
                found.unpositioned, 1,
                "the day at the lake has no GPS at all"
            );
            assert!(found.undated >= 3);
        }
        let events: Vec<Event> = Plan::files(seed, 4_000).unwrap().collect();
        assert_eq!(count(&events), 4_000);
        let found = check_events(&events);
        assert_eq!(found.jumps, 1);
        assert!(EVERY_KIND.iter().all(|kind| found.kinds.contains(kind)));
        let developed = Plan::developed(seed, 20_000, super::plan::SEPTEMBER);
        let events: Vec<Event> = developed.collect();
        assert_eq!(count(&events), 20_000);
        let found = check_events(&events);
        assert_eq!(found.jumps, 0);
        assert!(
            !found.kinds.contains(&MomentKind::Burst),
            "developed: no bursts"
        );
        assert!(found.undated >= 3);
        let years: Vec<i32> = events.iter().map(|e| e.start.year).collect();
        assert!(
            years[years.len() - 1] - years[0] >= 5,
            "over several years: {years:?}"
        );
    }
    assert!(Plan::images(1, super::plan::MOST_IMAGES + 1).is_err());
    assert!(Plan::files(1, 50).is_err());
}

/// Every row of every table of `schema` on `index`'s connection (its own `main`, or a catalog
/// attached to it), as text, in order.
fn dump(index: &luxforge_core::IndexDb, schema: &str) -> Vec<String> {
    let connection = index.connection();
    let mut tables = connection
        .prepare(&format!(
            "SELECT name FROM {schema}.sqlite_master
             WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name"
        ))
        .unwrap();
    let tables: Vec<String> = tables
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .map(|row| row.unwrap())
        .collect();
    let mut rows = Vec::new();
    for table in tables {
        let mut columns = connection
            .prepare("SELECT name FROM pragma_table_info(?1, ?2) ORDER BY cid")
            .unwrap();
        let columns: Vec<String> = columns
            .query_map([table.as_str(), schema], |row| row.get::<_, String>(0))
            .unwrap()
            .map(|row| row.unwrap())
            .collect();
        let text = columns
            .iter()
            .map(|column| format!("quote(\"{column}\")"))
            .collect::<Vec<_>>()
            .join(" || '|' || ");
        let mut select = connection
            .prepare(&format!(
                "SELECT {text} FROM {schema}.\"{table}\" ORDER BY 1"
            ))
            .unwrap();
        rows.extend(
            select
                .query_map([], |row| row.get::<_, String>(0))
                .unwrap()
                .map(|row| format!("{table}: {}", row.unwrap())),
        );
    }
    rows
}

/// A generated catalog and index in a scratch directory, removed when dropped.
struct Seeded {
    root: PathBuf,
    seed: u64,
}

impl Seeded {
    fn new(options: Options, label: &str) -> Self {
        let root = temp_dir(label);
        let seed = options.seed;
        run(&root.join("out"), &options).unwrap();
        Seeded { root, seed }
    }

    fn catalog(&self) -> PathBuf {
        self.root.join("out").join(CATALOG)
    }

    /// The index, with the catalog attached as `catalog`.
    fn index(&self) -> luxforge_core::IndexDb {
        let (index, opened) = luxforge_core::IndexDb::open(
            &luxforge_core::index_dir(&self.catalog()),
            &catalog_id(self.seed),
        )
        .unwrap();
        assert_eq!(opened, luxforge_core::IndexOpened::Opened);
        index
            .connection()
            .execute(
                "ATTACH DATABASE ?1 AS catalog",
                [self.catalog().to_str().unwrap()],
            )
            .unwrap();
        index
    }

    /// Every row of the index and the catalog.
    fn rows(&self) -> Vec<String> {
        let index = self.index();
        let mut rows = dump(&index, "main");
        rows.extend(dump(&index, "catalog"));
        rows
    }
}

impl Drop for Seeded {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn seeded(seed: u64, files: u32, assets: u32) -> Options {
    Options {
        seed,
        files: Some(files),
        assets: Some(assets),
        images: None,
    }
}

#[test]
fn the_same_seed_seeds_the_same_rows_and_another_seed_does_not() {
    let first = Seeded::new(seeded(7, 300, 900), "generate-catalog-rows-first");
    let again = Seeded::new(seeded(7, 300, 900), "generate-catalog-rows-again");
    let other = Seeded::new(seeded(8, 300, 900), "generate-catalog-rows-other");
    let rows = first.rows();
    assert!(rows.len() > 4 * 900 + 300, "{} rows", rows.len());
    assert!(rows == again.rows(), "the same seed seeded different rows");
    assert!(rows != other.rows());
}

/// One number the attached index and catalog answer.
fn count(index: &luxforge_core::IndexDb, query: &str) -> i64 {
    index
        .connection()
        .query_row(query, [], |row| row.get(0))
        .unwrap_or_else(|error| panic!("{query}: {error}"))
}

#[test]
fn a_generated_catalog_index_and_image_folder_open_with_the_core() {
    let seeded = Seeded::new(
        Options {
            images: Some(100),
            ..seeded(3, 400, 1_200)
        },
        "generate-catalog-open",
    );
    assert!(seeded.root.join("out/images/manifest.json").is_file());
    let service = luxforge_core::EditorService::open(&seeded.catalog()).unwrap();
    let assets = service.asset_ids(10).unwrap();
    assert_eq!(assets.len(), 10);
    for asset in &assets {
        let state = service.state(asset).unwrap();
        assert_eq!(state.current_entry.label, "Original");
        assert_eq!(&state.asset.id, asset);
    }
    let files: i64 = service
        .index()
        .unwrap()
        .connection()
        .query_row("SELECT count(*) FROM files", [], |row| row.get(0))
        .unwrap();
    assert_eq!(files, 400);
    drop(service);

    let index = seeded.index();
    let count = |query: &str| count(&index, query);
    assert_eq!(count("SELECT count(*) FROM catalog.assets"), 1_200);
    assert_eq!(
        count("SELECT count(DISTINCT source_kind) FROM catalog.assets"),
        2
    );
    for availability in ["available", "offline", "missing", "changed"] {
        assert!(
            count(&format!(
                "SELECT count(*) FROM catalog.assets WHERE availability = '{availability}'"
            )) > 0,
            "{availability}"
        );
    }
    assert!(count("SELECT count(*) FROM catalog.assets WHERE removed_ms IS NOT NULL") > 0);
    assert!(count("SELECT count(*) FROM catalog.assets WHERE develop_moment IS NOT NULL") > 1);
    // Some files are developed already: the same path, identity and capture header.
    let developed = "FROM catalog.assets a JOIN files f ON f.path = a.locator
                     JOIN catalog.capture c ON c.asset_row = a.row_id";
    assert!(count(&format!("SELECT count(*) {developed}")) > 0);
    assert_eq!(
        count(&format!(
            "SELECT count(*) {developed} WHERE a.file_identity
                 IS NOT 'unix:' || f.device || ':' || f.inode
             OR c.capture_ms IS NOT f.capture_ms OR c.local_text IS NOT f.local_text
             OR c.offset_minutes IS NOT f.offset_minutes OR c.make IS NOT f.make
             OR c.body_serial IS NOT f.body_serial OR c.lens IS NOT f.lens
             OR c.latitude IS NOT f.latitude OR c.exposure_time_s IS NOT f.exposure_time_s
             OR c.exposure_bias_ev IS NOT f.exposure_bias_ev OR c.width IS NOT f.width"
        )),
        0
    );
    // Picks are of indexed files not yet developed.
    assert!(count("SELECT count(*) FROM catalog.picks") > 0);
    assert_eq!(
        count(
            "SELECT count(*) FROM catalog.picks p LEFT JOIN files f ON f.path = p.path
             WHERE f.id IS NULL OR p.path IN (SELECT locator FROM catalog.assets)"
        ),
        0
    );
    assert!(count("SELECT count(*) FROM catalog.catalog_folders WHERE parent_id IS NOT NULL") > 0);
    assert!(
        count("SELECT count(*) FROM catalog.catalog_folders WHERE event_start_ms IS NOT NULL") > 0
    );
    assert_eq!(count("SELECT count(*) FROM catalog.collections"), 5);
    assert_eq!(
        count("SELECT count(*) FROM catalog.collections WHERE query_json IS NOT NULL"),
        1
    );
    assert_eq!(
        count("SELECT count(DISTINCT collection_id) FROM catalog.collection_members"),
        3
    );
    assert_eq!(count("SELECT count(*) FROM catalog.indexed_folders"), 2);
    assert_eq!(count("SELECT count(*) FROM catalog.volumes"), 3);
    assert_eq!(count("SELECT sum(file_count) FROM roots"), 400);
    assert_eq!(count("SELECT count(*) FROM roots WHERE offline = 1"), 1);
    assert_eq!(
        count("SELECT count(*) FROM files WHERE header_state = 'unreadable'"),
        2
    );
    assert!(count("SELECT count(*) FROM files WHERE header_state = 'pending'") > 0);
    assert!(count("SELECT count(*) FROM files WHERE thumb_len IS NOT NULL") > 0);
}
