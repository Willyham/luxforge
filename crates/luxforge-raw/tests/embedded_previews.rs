//! Embedded previews on authentic files: the owner's originals, and the per-camera inventory of
//! embedded thumbnails and previews that decides where the preview lane's development fallback
//! runs (`docs/research/embedded-previews.md`). Both only read the files; nothing is committed.
use luxforge_raw::{
    EmbeddedImage, EmbeddedPreview, EmbeddedPreviews, MAX_EMBEDDED_IMAGE_BYTES,
    MAX_EMBEDDED_READ_BUDGET, PreviewFormat, PreviewListing,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    fmt::Write as _,
    fs::{self, File},
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};

/// A preview whose long and short edges are each at least this fraction of the visible image's
/// is full size: the camera's JPEG is its default crop of the visible area, a few pixels short
/// of LibRaw's visible size on some bodies.
const FULL_SIZE: f64 = 0.95;
/// A usable preview whose long edge is below this many pixels is small: it draws a grid cell but
/// cannot fill a loupe on any current screen.
const THUMBNAIL_EDGE: u32 = 1024;

fn sha256(path: &Path) -> String {
    format!("{:x}", Sha256::digest(fs::read(path).expect("read source")))
}

/// The frame dimensions from a JPEG's first SOF marker, walking the marker segments only.
fn sof_dimensions(jpeg: &[u8]) -> Option<(u32, u32)> {
    let mut at = 2;
    while at + 4 <= jpeg.len() {
        if jpeg[at] != 0xff {
            return None;
        }
        let marker = jpeg[at + 1];
        if marker == 0xff {
            at += 1;
            continue;
        }
        if marker == 0x01 || (0xd0..=0xd8).contains(&marker) {
            at += 2;
            continue;
        }
        let length = usize::from(u16::from_be_bytes([jpeg[at + 2], jpeg[at + 3]]));
        if (0xc0..=0xcf).contains(&marker) && !matches!(marker, 0xc4 | 0xc8 | 0xcc) {
            let frame = jpeg.get(at + 5..at + 9)?;
            let height = u32::from(u16::from_be_bytes([frame[0], frame[1]]));
            let width = u32::from(u16::from_be_bytes([frame[2], frame[3]]));
            return Some((width, height));
        }
        if marker == 0xda || length < 2 {
            return None;
        }
        at += 2 + length;
    }
    None
}

/// What one extracted image turned out to be.
struct Extracted {
    preview: EmbeddedPreview,
    /// Its real dimensions: a JPEG's SOF, a bitmap's own.
    width: u32,
    height: u32,
    decodes: bool,
    note: Option<String>,
    trailing: bool,
    json: Value,
}

impl Extracted {
    fn pixels(&self) -> u64 {
        u64::from(self.width) * u64::from(self.height)
    }
    fn long_edge(&self) -> u32 {
        self.width.max(self.height)
    }
    fn usable(&self) -> bool {
        self.decodes
    }
    /// Its format and real dimensions.
    fn label(&self) -> String {
        format!(
            "{} {}×{}",
            format_name(self.preview.format),
            self.width,
            self.height
        )
    }
    /// The bytes it takes: a JPEG's stored length, a bitmap's pixels.
    fn bytes(&self) -> u64 {
        self.json["image_bytes"]
            .as_u64()
            .unwrap_or(self.preview.bytes)
    }
}

fn format_name(format: PreviewFormat) -> String {
    match format {
        PreviewFormat::Jpeg => "JPEG".into(),
        PreviewFormat::Bitmap { channels: 3 } => "RGB bitmap".into(),
        PreviewFormat::Bitmap { channels } => format!("{channels}-channel bitmap"),
        PreviewFormat::Bitmap16 { channels } => format!("{channels}-channel 16-bit bitmap"),
        PreviewFormat::Rollei => "Rollei bitmap".into(),
        PreviewFormat::H265 => "H.265".into(),
        PreviewFormat::JpegXl => "JPEG XL".into(),
        PreviewFormat::NotJpeg => "declared JPEG without SOI".into(),
        PreviewFormat::Kodak => "LibRaw Kodak kind".into(),
        PreviewFormat::DngYcbcr => "DNG YCbCr".into(),
        PreviewFormat::X3f => "X3F".into(),
        PreviewFormat::UnreadableBitmap => "unreadable bitmap".into(),
        PreviewFormat::Unknown => "unknown".into(),
    }
}

fn kilobytes(bytes: u64) -> String {
    if bytes >= 1_000_000 {
        format!("{:.1} MB", bytes as f64 / 1e6)
    } else {
        format!("{} KB", bytes.div_ceil(1000))
    }
}

/// Extract `preview` and judge it: a JPEG must decode with `image`, a bitmap is its pixels.
fn extract<R: luxforge_raw::RandomAccess>(
    previews: &mut EmbeddedPreviews<R>,
    preview: EmbeddedPreview,
    cancel: &AtomicBool,
) -> Extracted {
    let mut out = Extracted {
        preview,
        width: preview.width,
        height: preview.height,
        decodes: false,
        note: None,
        trailing: false,
        json: json!({}),
    };
    match previews.extract(preview.index, MAX_EMBEDDED_IMAGE_BYTES, cancel) {
        Err(error) => out.note = Some(error.to_string()),
        Ok(EmbeddedImage::Jpeg(bytes)) => {
            let sof = sof_dimensions(&bytes);
            out.trailing = !bytes.ends_with(&[0xff, 0xd9]);
            match image::load_from_memory_with_format(&bytes, image::ImageFormat::Jpeg) {
                Ok(decoded) => {
                    out.decodes = true;
                    out.width = decoded.width();
                    out.height = decoded.height();
                    if sof != Some((decoded.width(), decoded.height())) {
                        out.note = Some(format!("SOF {sof:?} differs from the decoded size"));
                    }
                }
                Err(error) => {
                    if let Some((width, height)) = sof {
                        out.width = width;
                        out.height = height;
                    }
                    out.note = Some(format!("does not decode: {error}"));
                }
            }
            out.json =
                json!({ "image_bytes": bytes.len(), "sof": sof, "ends_with_eoi": !out.trailing });
        }
        Ok(EmbeddedImage::Rgb8 {
            width,
            height,
            pixels,
        }) => {
            out.decodes = pixels.len() == 3 * width as usize * height as usize;
            out.width = width;
            out.height = height;
            out.json = json!({ "image_bytes": pixels.len() });
        }
    }
    let mut json = out.json.clone();
    json["index"] = json!(preview.index);
    json["format"] = serde_json::to_value(preview.format).unwrap();
    json["declared"] = json!([preview.width, preview.height]);
    json["declared_bytes"] = json!(preview.bytes);
    json["orientation"] = json!(preview.orientation);
    json["size"] = json!([out.width, out.height]);
    json["decodes"] = json!(out.decodes);
    json["note"] = json!(out.note);
    out.json = json;
    out
}

/// Whether `extracted` matches the visible image, long edge to long edge and short to short.
fn full_size(extracted: &Extracted, listing: &PreviewListing) -> (bool, f64) {
    let (long, short) = (
        f64::from(extracted.long_edge()),
        f64::from(extracted.width.min(extracted.height)),
    );
    let (visible_long, visible_short) = (
        f64::from(listing.width.max(listing.height)),
        f64::from(listing.width.min(listing.height)),
    );
    let fraction = long / visible_long;
    (
        fraction >= FULL_SIZE && short / visible_short >= FULL_SIZE,
        fraction,
    )
}

/// The owner's originals, read from their files: each lists a thumbnail and a larger JPEG, both
/// extract and decode, the largest is extracted after reading far less than the file, and the
/// files are unchanged. Set LUXFORGE_RAW_OWNER_DIR to the directory holding `nikon_z6.NEF`,
/// `fujifilm_x100vi.RAF` and `mavic_air_2s.DNG`.
#[test]
#[ignore = "requires explicit local authentic owner RAW fixture paths"]
fn embedded_owner_previews_extract_without_reading_whole_files() {
    let dir = env::var("LUXFORGE_RAW_OWNER_DIR").expect("owner fixture directory");
    let cancel = AtomicBool::new(false);
    for name in ["nikon_z6.NEF", "fujifilm_x100vi.RAF", "mavic_air_2s.DNG"] {
        let path = Path::new(&dir).join(name);
        let before = sha256(&path);
        let length = fs::metadata(&path).unwrap().len();
        let mut previews = EmbeddedPreviews::open(
            File::open(&path).unwrap(),
            MAX_EMBEDDED_READ_BUDGET,
            &cancel,
        )
        .unwrap_or_else(|error| panic!("{name}: {error}"));
        let listing = previews.listing().clone();
        let extracted: Vec<Extracted> = listing
            .previews
            .iter()
            .filter(|preview| preview.format.extractable())
            .map(|&preview| extract(&mut previews, preview, &cancel))
            .collect();
        for image in &extracted {
            assert!(
                image.decodes,
                "{name}: {:?} {:?}",
                image.preview, image.note
            );
        }
        let smallest = extracted.iter().min_by_key(|image| image.pixels()).unwrap();
        let largest = extracted.iter().max_by_key(|image| image.pixels()).unwrap();
        assert!(
            largest.pixels() > smallest.pixels(),
            "{name}: one size only"
        );
        assert_eq!(largest.preview.format, PreviewFormat::Jpeg, "{name}");
        assert_eq!(listing.largest_jpeg(), Some(&largest.preview), "{name}");
        // Open, list and extract only the largest, on a new handle.
        let mut fresh = EmbeddedPreviews::open(
            File::open(&path).unwrap(),
            MAX_EMBEDDED_READ_BUDGET,
            &cancel,
        )
        .unwrap();
        let listed = fresh.bytes_read();
        fresh
            .extract(largest.preview.index, MAX_EMBEDDED_IMAGE_BYTES, &cancel)
            .unwrap();
        let read = fresh.bytes_read();
        let (full, fraction) = full_size(largest, &listing);
        println!(
            "{name} ({} {}, {}×{} visible): thumbnail {}, largest {} (full size {full}, {:.2} of the long edge); {listed} bytes to list, {read} to extract the largest, of {length}",
            listing.make,
            listing.model,
            listing.width,
            listing.height,
            smallest.label(),
            largest.label(),
            fraction
        );
        assert!(read < length / 2, "{name}: {read} of {length}");
        assert!(listed < 1 << 20, "{name}: {listed} bytes to list");
        assert_eq!(sha256(&path), before, "{name} changed");
    }
}

/// Labels (make, model, mode) by raw.pixls.us id, from the raw.pixls.us index (`data` rows whose
/// link holds `getfile.php/<id>/`) and from lists of `{id, make, model, mode}` objects.
fn labels(paths: &str) -> BTreeMap<String, (String, String, String)> {
    let mut labels = BTreeMap::new();
    for path in env::split_paths(paths) {
        let text = fs::read_to_string(&path).expect("read labels");
        let value: Value = serde_json::from_str(&text).expect("label JSON");
        if let Some(rows) = value.get("data").and_then(Value::as_array) {
            for row in rows {
                let field = |i: usize| row[i].as_str().unwrap_or_default().to_string();
                let link = field(7);
                if let Some(start) = link.find("getfile.php/") {
                    let id: String = link[start + 12..]
                        .chars()
                        .take_while(char::is_ascii_digit)
                        .collect();
                    labels.insert(id, (field(0), field(1), field(2)));
                }
            }
        } else if let Some(rows) = value.as_array() {
            for row in rows {
                let field = |key: &str| row[key].as_str().unwrap_or_default().to_string();
                labels.insert(field("id"), (field("make"), field("model"), field("mode")));
            }
        }
    }
    labels
}

/// What a camera's files carry, from the largest preview that extracts and decodes: a JPEG at the
/// visible image's size (the 100% check can use it), a smaller one at least
/// [`THUMBNAIL_EDGE`] on its long edge (the loupe can), a smaller one still (only the grid can),
/// or none.
const CLASSES: [&str; 4] = ["full size", "reduced", "small only", "none usable"];

/// One file's answer, as the per-camera table compares them: formats and dimensions, not byte
/// counts, which vary with the picture.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Answer {
    class: usize,
    thumbnail: String,
    preview: String,
    full: String,
    fraction: String,
    notes: BTreeSet<String>,
}

/// One file of a camera: its mode, answer, the bytes read to list and to list and extract the
/// largest preview, and the bytes of its thumbnail and largest preview.
struct Row {
    mode: String,
    answer: Answer,
    listed: u64,
    extracted: Option<u64>,
    thumbnail_bytes: Option<u64>,
    preview_bytes: Option<u64>,
}

/// An image's cell: its label, and its bytes over the files.
fn cell(label: &str, bytes: String) -> String {
    if label == "–" {
        label.into()
    } else {
        format!("{label}, {bytes}")
    }
}

fn range(values: impl Iterator<Item = u64>) -> String {
    let values: Vec<u64> = values.collect();
    match (values.iter().min(), values.iter().max()) {
        (Some(low), Some(high)) => {
            let (low, high) = (kilobytes(*low), kilobytes(*high));
            if low == high {
                low
            } else {
                format!("{low}–{high}")
            }
        }
        _ => "–".into(),
    }
}

/// The per-camera inventory: every RAW file in the directories LUXFORGE_PREVIEW_DIRS lists (a
/// path list; each file once, whatever its name or link), opened from its file with the whole
/// read budget, every extractable image extracted, each JPEG decoded with `image`. It writes
/// `embedded-previews.json` (one record per file) and `embedded-previews.md` (one row per camera,
/// split by mode where the answer differs) to LUXFORGE_PREVIEW_OUTPUT, and labels raw.pixls.us
/// samples named `<id>.<ext>` with their mode from the JSON files LUXFORGE_PREVIEW_LABELS lists.
/// The files are only read, and each is checked unchanged.
#[test]
#[ignore = "requires the local RAW corpus"]
fn embedded_preview_inventory() {
    let dirs = env::var("LUXFORGE_PREVIEW_DIRS").expect("LUXFORGE_PREVIEW_DIRS");
    let output =
        PathBuf::from(env::var("LUXFORGE_PREVIEW_OUTPUT").expect("LUXFORGE_PREVIEW_OUTPUT"));
    let labels = env::var("LUXFORGE_PREVIEW_LABELS")
        .map(|paths| labels(&paths))
        .unwrap_or_default();
    let mut files = BTreeSet::new();
    for dir in env::split_paths(&dirs) {
        for entry in fs::read_dir(&dir).expect("list corpus directory") {
            let path = entry.unwrap().path();
            let data = path.extension().is_some_and(|extension| {
                matches!(
                    extension.to_string_lossy().to_ascii_lowercase().as_str(),
                    "json" | "tsv" | "txt" | "md"
                )
            });
            let hidden = path
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with('.'));
            if !data && !hidden {
                files.insert(fs::canonicalize(&path).unwrap());
            }
        }
    }
    let cancel = AtomicBool::new(false);
    let mut records = Vec::new();
    let mut cameras: BTreeMap<(String, String), Vec<Row>> = BTreeMap::new();
    let mut unopened = Vec::new();
    let mut observed = BTreeMap::<&str, BTreeSet<String>>::new();
    for path in &files {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let stem = path.file_stem().unwrap().to_string_lossy().into_owned();
        let label = labels.get(&stem).cloned();
        let mode = label
            .as_ref()
            .map_or_else(|| "owner original".to_string(), |label| label.2.clone());
        let length = fs::metadata(path).unwrap().len();
        let before = sha256(path);
        let mut record = json!({ "file": name, "length": length, "label": label });
        let opened =
            EmbeddedPreviews::open(File::open(path).unwrap(), MAX_EMBEDDED_READ_BUDGET, &cancel);
        let mut previews = match opened {
            Ok(previews) => previews,
            Err(error) => {
                record["open_error"] = json!(error.to_string());
                let camera =
                    label.map_or_else(|| name.clone(), |label| format!("{} {}", label.0, label.1));
                unopened.push(format!("{camera} ({mode}): {error}"));
                records.push(record);
                assert_eq!(sha256(path), before, "{name} changed");
                continue;
            }
        };
        let listing = previews.listing().clone();
        let camera = format!("{} {}", listing.make, listing.model);
        let listed = previews.bytes_read();
        record["listing"] = serde_json::to_value(&listing).unwrap();
        record["bytes_read_to_list"] = json!(listed);
        let mut extracted = Vec::new();
        let mut notes = BTreeSet::new();
        for &preview in &listing.previews {
            if preview.format.extractable() {
                extracted.push(extract(&mut previews, preview, &cancel));
            } else {
                notes.insert(format!(
                    "also lists {} {}×{}, not extracted",
                    format_name(preview.format),
                    preview.width,
                    preview.height
                ));
            }
        }
        record["extracted"] =
            Value::Array(extracted.iter().map(|image| image.json.clone()).collect());
        for image in &extracted {
            if let Some(note) = &image.note {
                notes.insert(format!("{}: {note}", image.label()));
            }
            if image.trailing {
                observed
                    .entry("a JPEG with bytes after its last EOI")
                    .or_default()
                    .insert(camera.clone());
            }
            let declared = (image.preview.width, image.preview.height);
            if image.preview.format == PreviewFormat::Jpeg
                && declared != (image.width, image.height)
            {
                let what = if declared.0 == 0 || declared.1 == 0 {
                    "a JPEG LibRaw lists without dimensions"
                } else {
                    "a JPEG whose listed dimensions differ from its SOF"
                };
                observed.entry(what).or_default().insert(format!(
                    "{camera} ({}×{} listed, {}×{} in its SOF)",
                    declared.0, declared.1, image.width, image.height
                ));
            }
            if image.decodes
                && image.width != image.height
                && (image.width < image.height) != (listing.width < listing.height)
            {
                observed
                    .entry("a preview stored across the sensor's orientation")
                    .or_default()
                    .insert(camera.clone());
            }
        }
        let usable: Vec<&Extracted> = extracted.iter().filter(|image| image.usable()).collect();
        let thumbnail = usable
            .iter()
            .min_by_key(|image| (image.pixels(), image.bytes()))
            .copied();
        let largest = usable
            .iter()
            .max_by_key(|image| (image.pixels(), image.bytes()))
            .copied();
        // The listing's own choice, by stored length, is the largest in pixels.
        if let Some(largest) = largest.filter(|image| image.preview.format == PreviewFormat::Jpeg) {
            assert_eq!(listing.largest_jpeg(), Some(&largest.preview), "{name}");
        }
        let (class, full, fraction, read_largest) = match largest {
            None => {
                notes.insert(if listing.previews.is_empty() {
                    "no preview listed".into()
                } else {
                    "no preview extracts and decodes".into()
                });
                (3, "–".to_string(), "–".to_string(), None)
            }
            Some(largest) => {
                let (full, fraction) = full_size(largest, &listing);
                let class = if full {
                    0
                } else if largest.long_edge() >= THUMBNAIL_EDGE {
                    1
                } else {
                    2
                };
                // Open, list and extract only the largest, on a new handle.
                let mut fresh = EmbeddedPreviews::open(
                    File::open(path).unwrap(),
                    MAX_EMBEDDED_READ_BUDGET,
                    &cancel,
                )
                .unwrap();
                fresh
                    .extract(largest.preview.index, MAX_EMBEDDED_IMAGE_BYTES, &cancel)
                    .unwrap();
                let full = if full { "yes" } else { "no" };
                (
                    class,
                    full.to_string(),
                    format!("{fraction:.2}"),
                    Some(fresh.bytes_read()),
                )
            }
        };
        record["class"] = json!(CLASSES[class]);
        record["thumbnail"] = json!(thumbnail.map(|image| image.json.clone()));
        record["largest"] = json!(largest.map(|image| image.json.clone()));
        record["full_size"] = json!(full);
        record["long_edge_fraction"] = json!(fraction);
        record["bytes_read_to_list_and_extract_largest"] = json!(read_largest);
        let answer = Answer {
            class,
            thumbnail: thumbnail.map_or("–".into(), |image| image.label()),
            preview: largest.map_or("–".into(), |image| image.label()),
            full,
            fraction,
            notes,
        };
        cameras
            .entry((listing.make.clone(), listing.model.clone()))
            .or_default()
            .push(Row {
                mode,
                answer,
                listed,
                extracted: read_largest,
                thumbnail_bytes: thumbnail.map(Extracted::bytes),
                preview_bytes: largest.map(Extracted::bytes),
            });
        records.push(record);
        drop(previews);
        assert_eq!(sha256(path), before, "{name} changed");
    }

    // One row per camera, or per mode where a camera's files answer differently.
    let mut table = String::from(
        "| Camera | Files | Thumbnail | Largest usable preview | Full size | Long edge / visible | Read to list | Read to list and extract it | Notes |\n| --- | --- | --- | --- | --- | --- | --- | --- | --- |\n",
    );
    let mut classes: BTreeMap<usize, BTreeSet<String>> = BTreeMap::new();
    for ((make, model), rows) in &cameras {
        let answers: BTreeSet<&Answer> = rows.iter().map(|row| &row.answer).collect();
        let mut groups: BTreeMap<String, Vec<&Row>> = BTreeMap::new();
        for row in rows {
            let key = if answers.len() == 1 {
                format!("{make} {model}")
            } else {
                format!("{make} {model} ({})", row.mode)
            };
            groups.entry(key).or_default().push(row);
        }
        for (camera, group) in groups {
            let answer = &group[0].answer;
            assert!(
                group.iter().all(|row| &row.answer == answer),
                "{camera}: one mode, two answers"
            );
            let notes: Vec<&str> = answer.notes.iter().map(String::as_str).collect();
            writeln!(
                table,
                "| {camera} | {} | {} | {} | {} | {} | {} | {} | {} |",
                group.len(),
                cell(
                    &answer.thumbnail,
                    range(group.iter().filter_map(|row| row.thumbnail_bytes))
                ),
                cell(
                    &answer.preview,
                    range(group.iter().filter_map(|row| row.preview_bytes))
                ),
                answer.full,
                answer.fraction,
                range(group.iter().map(|row| row.listed)),
                range(group.iter().filter_map(|row| row.extracted)),
                notes.join("; ")
            )
            .unwrap();
            classes.entry(answer.class).or_default().insert(camera);
        }
    }
    let mut report = format!(
        "{} files, {} cameras, {} not opened.\n\n",
        files.len(),
        cameras.len(),
        unopened.len()
    );
    for (class, members) in &classes {
        writeln!(
            report,
            "- **{}** ({}): {}",
            CLASSES[*class],
            members.len(),
            members.iter().cloned().collect::<Vec<_>>().join(", ")
        )
        .unwrap();
    }
    let listed: Vec<u64> = cameras.values().flatten().map(|row| row.listed).collect();
    writeln!(
        report,
        "\nOpening and listing read {} to {} a file, {} on average.\n\n{table}",
        kilobytes(*listed.iter().min().unwrap_or(&0)),
        kilobytes(*listed.iter().max().unwrap_or(&0)),
        kilobytes(listed.iter().sum::<u64>() / listed.len().max(1) as u64)
    )
    .unwrap();
    if !unopened.is_empty() {
        report.push_str("Not opened:\n\n");
        for line in &unopened {
            writeln!(report, "- {line}").unwrap();
        }
        report.push('\n');
    }
    if !observed.is_empty() {
        report.push_str("Observed:\n\n");
        for (what, cameras) in &observed {
            writeln!(
                report,
                "- {what}: {}",
                cameras.iter().cloned().collect::<Vec<_>>().join(", ")
            )
            .unwrap();
        }
    }
    fs::create_dir_all(&output).unwrap();
    fs::write(
        output.join("embedded-previews.json"),
        serde_json::to_string_pretty(&records).unwrap(),
    )
    .unwrap();
    fs::write(output.join("embedded-previews.md"), &report).unwrap();
    println!("{report}");
}
