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

/// Every property the plan promises the event and moment rules, checked over a whole plan.
fn check_plan(events: &[Event]) {
    let mut kinds = Vec::new();
    let mut jumps = 0;
    let mut previous: Option<Frame> = None;
    for event in events.iter().filter(|e| e.place.is_some()) {
        let mut frames = event.frames.clone();
        frames.sort_by_key(|f| f.utc().unwrap());
        let (first, last) = (frames[0], *frames.last().unwrap());
        if let Some(before) = previous {
            let gap = first.utc().unwrap() - before.utc().unwrap();
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
                jumps += 1;
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
        // One body's consecutive moments are over 2 s apart.
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
        for frames in moments.values() {
            let kind = frames[0].kind;
            kinds.push(kind);
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
    assert_eq!(jumps, 1, "one same-day jump of over 25 km");
    for kind in [
        MomentKind::Single,
        MomentKind::Burst,
        MomentKind::Bracket(BracketKind::WithBias),
        MomentKind::Bracket(BracketKind::WithoutBias),
        MomentKind::Bracket(BracketKind::MetadataLess),
    ] {
        assert!(kinds.contains(&kind), "no {kind:?}");
    }
    assert!(
        events
            .iter()
            .any(|e| e.place.is_some() && !e.positioned() && e.frames.len() > 1),
        "an event with no GPS at all"
    );
    let undated = events.iter().find(|e| e.place.is_none()).unwrap();
    assert!(undated.frames.iter().all(|f| f.time.is_none()));
}

#[test]
fn every_plan_keeps_the_event_and_moment_rules_and_its_count() {
    for seed in 1..=6 {
        for count in [
            super::plan::september().iter().map(|s| s.minimum()).sum(),
            150,
            700,
            super::plan::MOST_IMAGES,
        ] {
            let events: Vec<Event> = Plan::images(seed, count).unwrap().collect();
            assert_eq!(
                events.iter().map(|e| e.frames.len() as u32).sum::<u32>(),
                count
            );
            let days: Vec<usize> = events.iter().map(|e| e.days().len()).collect();
            assert_eq!(days, [2, 1, 1, 1, 3, 0], "seed {seed}, {count} frames");
            for event in &events {
                // Moments start in capture order, so no day ran into the next.
                let mut starts: BTreeMap<u32, i64> = BTreeMap::new();
                for frame in event.frames.iter().filter(|f| f.time.is_some()) {
                    starts.entry(frame.moment).or_insert(frame.utc().unwrap());
                }
                let starts: Vec<i64> = starts.into_values().collect();
                assert!(starts.windows(2).all(|s| s[0] <= s[1]), "{}", event.label);
            }
            check_plan(&events);
        }
    }
    assert!(Plan::images(1, super::plan::MOST_IMAGES + 1).is_err());
}
