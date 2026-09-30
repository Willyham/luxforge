//! The header reader on authentic files, which are never committed and only ever read:
//!
//! - `LUXFORGE_RAW_OWNER_DIR`: the owner's `nikon_z6.NEF`, `fujifilm_x100vi.RAF` and
//!   `mavic_air_2s.DNG`.
//! - `LUXFORGE_RAW_CORPUS_DIR` and `LUXFORGE_RAW_POPULAR_DIR`: raw.pixls.us samples named
//!   `<id>.<EXT>`, each directory with LibRaw's `results.json` (make, model, EXIF orientation and
//!   default crop per id).
//! - `LUXFORGE_JPEG_DIR`: camera JPEGs.
//!
//! For every file the bounded read (of the file itself) equals the whole-file read and is not
//! capped; the typed values agree with `kamadak-exif` wherever it reads the container (JPEG, TIFF,
//! and ORF, RW2, RAF and CR3 through their TIFF structures) and with LibRaw's make, model and
//! orientation. A row per file and a table per format are printed:
//!
//! ```text
//! LUXFORGE_RAW_OWNER_DIR=/path/to/owner/raw LUXFORGE_RAW_CORPUS_DIR=/path/to/corpus \
//!   LUXFORGE_RAW_POPULAR_DIR=/path/to/popular LUXFORGE_JPEG_DIR=/path/to/jpeg \
//!   cargo test --release -p luxforge-core --lib metadata::header::authentic -- --ignored --nocapture
//! ```

use super::{Container, FileHeader, HeaderRead, ThumbnailFormat, TimeSource, read_header};
use exif::{Context, In, Reader, Value};
use luxforge_testbase::Distribution;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::Cursor;
use std::path::{Path, PathBuf};

/// The directory `variable` names, or a `skipped:` note when it is unset or absent.
fn directory(variable: &str) -> Option<PathBuf> {
    let Some(path) = std::env::var_os(variable).map(PathBuf::from) else {
        eprintln!("skipped: {variable} is not set");
        return None;
    };
    if !path.is_dir() {
        eprintln!(
            "skipped: {variable} ({}) is not a directory",
            path.display()
        );
        return None;
    }
    Some(path)
}

/// What LibRaw recorded for one sample.
struct LibRaw {
    make: String,
    model: String,
    orientation: u64,
    crop: (u64, u64),
}

fn libraw_records(dir: &Path) -> BTreeMap<String, LibRaw> {
    let Ok(text) = std::fs::read_to_string(dir.join("results.json")) else {
        return BTreeMap::new();
    };
    let rows: Vec<serde_json::Value> = serde_json::from_str(&text).expect("results.json");
    rows.iter()
        .filter_map(|row| {
            let metadata = row.get("metadata")?;
            let crop = metadata.get("default_crop")?;
            Some((
                row["id"].as_str()?.to_owned(),
                LibRaw {
                    make: row["make"].as_str()?.to_owned(),
                    model: row["model"].as_str()?.to_owned(),
                    orientation: metadata["exif_orientation"].as_u64()?,
                    crop: (crop["width"].as_u64()?, crop["height"].as_u64()?),
                },
            ))
        })
        .collect()
}

/// One file's result.
struct Row {
    name: String,
    format: String,
    read: HeaderRead,
    disagreements: Vec<String>,
    libraw_crop: Option<(u64, u64)>,
}

/// Read `path` bounded (from the file) and whole (from memory); they must be equal and the bounded
/// read not capped. Then compare with the independent readers.
fn check(path: &Path, libraw: Option<&LibRaw>) -> Row {
    let bytes = std::fs::read(path).expect("read the sample");
    let whole = FileHeader::from_bytes(&bytes);
    let mut file = File::open(path).expect("open the sample");
    let read = read_header(&mut file, bytes.len() as u64).expect("bounded read");
    let name = path.file_name().unwrap().to_string_lossy().into_owned();
    assert!(!read.capped, "{name}: the bounded read was capped");
    assert_eq!(read.header, whole, "{name}: bounded and whole reads differ");
    let cursor = read_header(&mut Cursor::new(&bytes), bytes.len() as u64).unwrap();
    assert_eq!(cursor, read, "{name}: a cursor reads what the file does");
    let independent = independent(&bytes, &read.header);
    let mut disagreements = independent.compare(&read.header);
    // The comparison is only evidence when kamadak read the file's camera and time too.
    if independent.text(Table::Primary, 0x010f).is_none() || independent.time().is_none() {
        disagreements.push("kamadak read no make or no time".to_owned());
    }
    if let Some(libraw) = libraw {
        disagreements.extend(compare_libraw(&read.header, libraw));
    }
    let format = path
        .extension()
        .map(|ext| ext.to_string_lossy().to_uppercase())
        .unwrap_or_default();
    Row {
        name,
        format,
        read,
        disagreements,
        libraw_crop: libraw.map(|libraw| libraw.crop),
    }
}

fn normalized(text: &str) -> String {
    text.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// A camera name compared with LibRaw's: letters and digits only, lower case, its spellings of
/// "Mark II" and "Mark III" and its name for OM Digital Solutions made one.
fn named(text: &str) -> String {
    normalized(text)
        .replace("omdigitalsolutions", "omsystem")
        .replace("markiii", "m3")
        .replace("markii", "m2")
}

/// LibRaw names cameras by its own table, not by the EXIF text: Pentax for "RICOH IMAGING
/// COMPANY, LTD." (the brand is in the model), "OM System" for "OM Digital Solutions", and
/// "Mark II" where Canon writes "m2". Those are the same camera, so they agree.
fn compare_libraw(header: &FileHeader, libraw: &LibRaw) -> Vec<String> {
    let mut out = Vec::new();
    let related = |ours: Option<&String>, theirs: &str| {
        ours.is_some_and(|ours| {
            let (ours, theirs) = (named(ours), named(theirs));
            ours.contains(&theirs) || theirs.contains(&ours)
        })
    };
    if !related(header.make.as_ref(), &libraw.make) && !related(header.model.as_ref(), &libraw.make)
    {
        out.push(format!(
            "make {:?} vs LibRaw {:?}",
            header.make, libraw.make
        ));
    }
    if !related(header.model.as_ref(), &libraw.model) {
        out.push(format!(
            "model {:?} vs LibRaw {:?}",
            header.model, libraw.model
        ));
    }
    if header.orientation.map(u64::from) != Some(libraw.orientation) {
        out.push(format!(
            "orientation {:?} vs LibRaw {}",
            header.orientation, libraw.orientation
        ));
    }
    out
}

/// Which table a kamadak field is read as.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Table {
    Primary,
    Exif,
    Gps,
    Thumbnail,
}

/// The fields an independent reader found, first found wins across the TIFF structures given, and
/// where each structure starts in the file.
#[derive(Default)]
struct Independent {
    fields: BTreeMap<(Table, u16), Value>,
    /// IFD1's JPEG as an absolute offset and length, from the first structure that has one.
    thumbnail: Option<(u64, u64)>,
}

impl Independent {
    /// Add the fields of one TIFF structure starting at `base`; `table` overrides the tables of
    /// its IFD0 (a CR3's `CMT2` is the Exif IFD, `CMT4` the GPS IFD).
    fn add(&mut self, tiff: Vec<u8>, base: u64, table: Option<Table>) {
        let exif = match Reader::new().continue_on_error(true).read_raw(tiff) {
            Ok(exif) => exif,
            Err(error) => match error.distill_partial_result(|_| {}) {
                Ok(exif) => exif,
                Err(_) => return,
            },
        };
        for field in exif.fields() {
            let kind = if field.ifd_num == In::THUMBNAIL {
                Table::Thumbnail
            } else if field.ifd_num != In::PRIMARY {
                continue;
            } else if let Some(table) = table {
                table
            } else {
                match field.tag.context() {
                    Context::Tiff => Table::Primary,
                    Context::Exif => Table::Exif,
                    Context::Gps => Table::Gps,
                    _ => continue,
                }
            };
            self.fields
                .entry((kind, field.tag.number()))
                .or_insert_with(|| field.value.clone());
        }
        let offset = exif
            .fields()
            .find(|field| field.ifd_num == In::THUMBNAIL && field.tag.number() == 0x0201);
        let length = exif
            .fields()
            .find(|field| field.ifd_num == In::THUMBNAIL && field.tag.number() == 0x0202);
        if let (None, Some(offset), Some(length)) = (self.thumbnail, offset, length)
            && let (Some(offset), Some(length)) =
                (offset.value.get_uint(0), length.value.get_uint(0))
        {
            self.thumbnail = Some((base + u64::from(offset), u64::from(length)));
        }
    }

    fn get(&self, table: Table, tag: u16) -> Option<&Value> {
        self.fields.get(&(table, tag))
    }

    fn text(&self, table: Table, tag: u16) -> Option<String> {
        match self.get(table, tag)? {
            Value::Ascii(parts) => {
                let text = String::from_utf8_lossy(parts.first()?).trim().to_owned();
                (!text.is_empty()).then_some(text)
            }
            _ => None,
        }
    }

    fn rational(&self, table: Table, tag: u16) -> Option<(u32, u32)> {
        match self.get(table, tag)? {
            Value::Rational(values) => values.first().map(|value| (value.num, value.denom)),
            _ => None,
        }
    }

    fn uint(&self, table: Table, tag: u16) -> Option<u32> {
        self.get(table, tag)?.get_uint(0)
    }

    /// Compare with `header`: a disagreement is a value both have that differs, or one only the
    /// independent reader has.
    fn compare(&self, header: &FileHeader) -> Vec<String> {
        let mut out = Vec::new();
        let mut same = |what: &str, ours: Option<String>, theirs: Option<String>| {
            if theirs.is_some() && ours != theirs {
                out.push(format!("{what}: ours {ours:?}, kamadak {theirs:?}"));
            }
        };
        same(
            "make",
            header.make.clone(),
            self.text(Table::Primary, 0x010f),
        );
        same(
            "model",
            header.model.clone(),
            self.text(Table::Primary, 0x0110),
        );
        same(
            "lens make",
            header.lens_make.clone(),
            self.text(Table::Exif, 0xa433),
        );
        same(
            "lens model",
            header.lens_model.clone(),
            self.text(Table::Exif, 0xa434),
        );
        same(
            "body serial",
            header.body_serial.clone(),
            self.text(Table::Exif, 0xa431)
                .or_else(|| self.text(Table::Primary, 0xc62f)),
        );
        let pair = |value: Option<(u32, u32)>| value.map(|(num, den)| format!("{num}/{den}"));
        same(
            "exposure time",
            pair(header.exposure_time.map(|value| (value.num, value.den))),
            pair(self.rational(Table::Exif, 0x829a)),
        );
        same(
            "f-number",
            pair(header.f_number.map(|value| (value.num, value.den))),
            pair(self.rational(Table::Exif, 0x829d)),
        );
        same(
            "focal length",
            pair(header.focal_length.map(|value| (value.num, value.den))),
            pair(self.rational(Table::Exif, 0x920a)),
        );
        same(
            "focal length 35",
            header.focal_length_35mm.map(|value| value.to_string()),
            self.uint(Table::Exif, 0xa405)
                .filter(|value| *value > 0)
                .map(|value| value.to_string()),
        );
        same(
            "ISO",
            header.iso.map(|value| value.to_string()),
            self.uint(Table::Exif, 0x8827)
                .filter(|value| (1..65_535).contains(value))
                .map(|value| value.to_string()),
        );
        let bias = match self.get(Table::Exif, 0x9204) {
            Some(Value::SRational(values)) => {
                values.first().map(|v| format!("{}/{}", v.num, v.denom))
            }
            _ => None,
        };
        same(
            "exposure bias",
            header
                .exposure_bias
                .map(|value| format!("{}/{}", value.num, value.den)),
            bias,
        );
        same(
            "orientation",
            header.orientation.map(|value| value.to_string()),
            self.uint(Table::Primary, 0x0112)
                .filter(|value| (1..=8).contains(value))
                .map(|value| value.to_string()),
        );
        same("capture time", time(header), self.time());
        same("GPS", gps(header), self.gps());
        if let (Some(theirs), Some(ours)) = (self.thumbnail, header.thumbnail)
            && matches!(ours.format, ThumbnailFormat::Jpeg { .. })
            && (ours.offset, ours.length) != theirs
        {
            out.push(format!(
                "thumbnail: ours {}+{}, kamadak IFD1 {}+{}",
                ours.offset, ours.length, theirs.0, theirs.1
            ));
        }
        out
    }

    /// DateTimeOriginal, else DateTimeDigitized, else DateTime, with its paired tags, through
    /// kamadak's own parsers.
    fn time(&self) -> Option<String> {
        [
            (Table::Exif, 0x9003, 0x9291, 0x9011, "Original"),
            (Table::Exif, 0x9004, 0x9292, 0x9012, "Digitized"),
            (Table::Primary, 0x0132, 0x9290, 0x9010, "Modified"),
        ]
        .into_iter()
        .find_map(|(table, tag, subsec, offset, source)| {
            let Some(Value::Ascii(parts)) = self.get(table, tag) else {
                return None;
            };
            let mut time = exif::DateTime::from_ascii(parts.first()?).ok()?;
            if let Some(Value::Ascii(parts)) = self.get(Table::Exif, subsec) {
                let _ = time.parse_subsec(parts.first()?);
            }
            if let Some(Value::Ascii(parts)) = self.get(Table::Exif, offset) {
                let _ = time.parse_offset(parts.first()?);
            }
            Some(format!(
                "{:04}:{:02}:{:02} {:02}:{:02}:{:02} {:?} {:?} {source}",
                time.year,
                time.month,
                time.day,
                time.hour,
                time.minute,
                time.second,
                time.nanosecond,
                time.offset
            ))
        })
    }

    fn gps(&self) -> Option<String> {
        let degrees = |tag| match self.get(Table::Gps, tag) {
            Some(Value::Rational(values)) if !values.is_empty() => Some(
                values
                    .iter()
                    .zip([1.0, 60.0, 3600.0])
                    .map(|(value, scale)| value.to_f64() / scale)
                    .sum::<f64>(),
            ),
            _ => None,
        };
        if self.text(Table::Gps, 0x0009).as_deref() == Some("V") {
            return None;
        }
        let (latitude, longitude) = (degrees(0x0002)?, degrees(0x0004)?);
        if latitude == 0.0 && longitude == 0.0 {
            return None;
        }
        let latitude = match self.text(Table::Gps, 0x0001)?.as_str() {
            "N" => latitude,
            "S" => -latitude,
            _ => return None,
        };
        let longitude = match self.text(Table::Gps, 0x0003)?.as_str() {
            "E" => longitude,
            "W" => -longitude,
            _ => return None,
        };
        Some(format!("{latitude:.9} {longitude:.9}"))
    }
}

fn time(header: &FileHeader) -> Option<String> {
    let time = header.capture_time?;
    let local = time.local;
    let source = match time.source {
        TimeSource::Original => "Original",
        TimeSource::Digitized => "Digitized",
        TimeSource::Modified => "Modified",
    };
    Some(format!(
        "{:04}:{:02}:{:02} {:02}:{:02}:{:02} {:?} {:?} {source}",
        local.year,
        local.month,
        local.day,
        local.hour,
        local.minute,
        local.second,
        time.subsec_nanos,
        time.offset_minutes
    ))
}

fn gps(header: &FileHeader) -> Option<String> {
    header
        .gps
        .map(|gps| format!("{:.9} {:.9}", gps.latitude, gps.longitude))
}

fn be32(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

/// The first `Exif` APP1's TIFF in the JPEG `bytes[start..]`, and where it starts.
fn jpeg_tiff(bytes: &[u8], start: usize) -> Option<(Vec<u8>, u64)> {
    let jpeg = bytes.get(start..)?;
    let mut at = 2;
    while at + 4 <= jpeg.len() && jpeg[at] == 0xff {
        let length = usize::from(u16::from_be_bytes([jpeg[at + 2], jpeg[at + 3]]));
        let payload = jpeg.get(at + 4..at + 2 + length)?;
        if jpeg[at + 1] == 0xe1 && payload.starts_with(b"Exif\0\0") {
            return Some((payload[6..].to_vec(), (start + at + 10) as u64));
        }
        if jpeg[at + 1] == 0xda {
            return None;
        }
        at += 2 + length;
    }
    None
}

/// The Canon metadata box's user type, 85c0b687-820f-11e0-8111-f4ce462b6a48.
const CANON_UUID: [u8; 16] = [
    0x85, 0xc0, 0xb6, 0x87, 0x82, 0x0f, 0x11, 0xe0, 0x81, 0x11, 0xf4, 0xce, 0x46, 0x2b, 0x6a, 0x48,
];

/// The CR3 `moov` Canon box's `CMTn` children, by name.
fn cr3_boxes(bytes: &[u8]) -> BTreeMap<[u8; 4], (usize, usize)> {
    let mut out = BTreeMap::new();
    let walk = |start: usize, end: usize| {
        let mut children = Vec::new();
        let mut at = start;
        while at + 8 <= end {
            let size = be32(bytes, at).unwrap() as usize;
            let kind: [u8; 4] = bytes[at + 4..at + 8].try_into().unwrap();
            if size < 8 || at + size > end {
                break;
            }
            children.push((kind, at, at + size));
            at += size;
        }
        children
    };
    for (kind, start, end) in walk(0, bytes.len()) {
        if &kind != b"moov" {
            continue;
        }
        for (kind, start, end) in walk(start + 8, end) {
            if &kind == b"uuid" && bytes[start + 8..start + 24] == CANON_UUID {
                for (kind, start, end) in walk(start + 24, end) {
                    out.insert(kind, (start + 8, end));
                }
            }
        }
    }
    out
}

/// What kamadak reads of the file's TIFF structures.
fn independent(bytes: &[u8], header: &FileHeader) -> Independent {
    let mut found = Independent::default();
    match header.container {
        Container::Jpeg => {
            if let Some((tiff, base)) = jpeg_tiff(bytes, 0) {
                found.add(tiff, base, None);
            }
        }
        Container::Tiff => found.add(bytes.to_vec(), 0, None),
        Container::Orf | Container::Rw2 => {
            // The same structure with the standard magic, which kamadak requires.
            let mut copy = bytes.to_vec();
            let magic = if copy[0] == b'M' { [0, 42] } else { [42, 0] };
            copy[2..4].copy_from_slice(&magic);
            found.add(copy, 0, None);
            if header.container == Container::Rw2
                && let Some((tiff, base)) = rw2_jpeg(bytes).and_then(|at| jpeg_tiff(bytes, at))
            {
                found.add(tiff, base, None);
            }
        }
        Container::Raf => {
            let at = be32(bytes, 84).unwrap() as usize;
            if let Some((tiff, base)) = jpeg_tiff(bytes, at) {
                found.add(tiff, base, None);
            }
        }
        Container::Cr3 => {
            let boxes = cr3_boxes(bytes);
            for (name, table) in [
                (b"CMT1", None),
                (b"CMT2", Some(Table::Exif)),
                (b"CMT4", Some(Table::Gps)),
            ] {
                if let Some(&(start, end)) = boxes.get(name) {
                    found.add(bytes[start..end].to_vec(), start as u64, table);
                }
            }
        }
        Container::Unknown => {}
    }
    found
}

/// A RW2's embedded JPEG (IFD0's 0x002e), found by a plain walk of IFD0.
fn rw2_jpeg(bytes: &[u8]) -> Option<usize> {
    let u16_at = |at: usize| Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?));
    let u32_at = |at: usize| Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?));
    let ifd0 = u32_at(4)? as usize;
    (0..usize::from(u16_at(ifd0)?)).find_map(|index| {
        let entry = ifd0 + 2 + index * 12;
        (u16_at(entry)? == 0x002e).then(|| u32_at(entry + 8).map(|at| at as usize))?
    })
}

fn yes(value: bool) -> &'static str {
    if value { "y" } else { "-" }
}

fn print_row(row: &Row) {
    let header = &row.read.header;
    let thumbnail = match header.thumbnail {
        None => "none".to_owned(),
        Some(thumbnail) => match thumbnail.format {
            ThumbnailFormat::Jpeg { size } => format!(
                "JPEG {} {}B",
                size.map_or("?".to_owned(), |size| format!(
                    "{}x{}",
                    size.width, size.height
                )),
                thumbnail.length
            ),
            ThumbnailFormat::Rgb8 { size } => format!("RGB {}x{}", size.width, size.height),
        },
    };
    let dimensions = header.dimensions.map_or("-".to_owned(), |size| {
        format!("{}x{}", size.width, size.height)
    });
    let libraw = row.libraw_crop.map_or(String::new(), |(width, height)| {
        format!(" libraw {width}x{height}")
    });
    eprintln!(
        "{:<10} {:<5} {:<20} {:<18} t:{} ss:{} off:{} ser:{} lens:{} bias:{} gps:{} iso:{:<6} or:{} {:<22} dims {}{} {}B/{}r",
        row.name,
        row.format,
        header.make.as_deref().unwrap_or("-"),
        header.model.as_deref().unwrap_or("-"),
        header.capture_time.map_or("-", |time| match time.source {
            TimeSource::Original => "O",
            TimeSource::Digitized => "D",
            TimeSource::Modified => "M",
        }),
        yes(header
            .capture_time
            .is_some_and(|time| time.subsec_nanos.is_some())),
        yes(header
            .capture_time
            .is_some_and(|time| time.offset_minutes.is_some())),
        header.body_serial.as_deref().unwrap_or("-"),
        yes(header.lens_model.is_some()),
        yes(header.exposure_bias.is_some()),
        yes(header.gps.is_some()),
        header.iso.map_or("-".to_owned(), |iso| iso.to_string()),
        header
            .orientation
            .map_or("-".to_owned(), |value| value.to_string()),
        thumbnail,
        dimensions,
        libraw,
        row.read.bytes_read,
        row.read.reads,
    );
    for disagreement in &row.disagreements {
        eprintln!("    disagreement: {disagreement}");
    }
}

/// A table per format: coverage of each field and the bytes and reads used.
fn print_summary(rows: &[Row]) {
    let mut formats: BTreeMap<&str, Vec<&Row>> = BTreeMap::new();
    for row in rows {
        formats.entry(&row.format).or_default().push(row);
    }
    eprintln!(
        "| Format | Files | Time | Subsec | Offset | Serial | Lens | Bias | GPS | Thumbnail | Bytes max | Bytes p50 | Reads max | Reads p50 |"
    );
    eprintln!(
        "| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |"
    );
    for (format, rows) in formats {
        let count = |test: &dyn Fn(&FileHeader) -> bool| {
            rows.iter().filter(|row| test(&row.read.header)).count()
        };
        let bytes = Distribution::of(rows.iter().map(|row| row.read.bytes_read as f64)).unwrap();
        let reads = Distribution::of(rows.iter().map(|row| f64::from(row.read.reads))).unwrap();
        eprintln!(
            "| {format} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
            rows.len(),
            count(&|header| header.capture_time.is_some()),
            count(&|header| header
                .capture_time
                .is_some_and(|time| time.subsec_nanos.is_some())),
            count(&|header| header
                .capture_time
                .is_some_and(|time| time.offset_minutes.is_some())),
            count(&|header| header.body_serial.is_some()),
            count(&|header| header.lens_model.is_some()),
            count(&|header| header.exposure_bias.is_some()),
            count(&|header| header.gps.is_some()),
            count(&|header| header.thumbnail.is_some()),
            bytes.percentile(100),
            bytes.percentile(50),
            reads.percentile(100),
            reads.percentile(50),
        );
    }
}

/// Check every file in `dir` (its RAW or JPEG files by extension), with LibRaw's records when the
/// directory has them.
fn check_directory(dir: &Path, extensions: &[&str]) -> Vec<Row> {
    let records = libraw_records(dir);
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
        .expect("list the directory")
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.extension().is_some_and(|ext| {
                extensions.contains(&ext.to_string_lossy().to_lowercase().as_str())
            })
        })
        .collect();
    paths.sort();
    paths
        .iter()
        .map(|path| {
            let id = path.file_stem().unwrap().to_string_lossy();
            check(path, records.get(id.as_ref()))
        })
        .collect()
}

const RAW_EXTENSIONS: [&str; 9] = [
    "arw", "cr2", "cr3", "dng", "nef", "orf", "pef", "raf", "rw2",
];

fn report(rows: &[Row]) {
    for row in rows {
        print_row(row);
    }
    print_summary(rows);
    let disagreeing: Vec<&str> = rows
        .iter()
        .filter(|row| !row.disagreements.is_empty())
        .map(|row| row.name.as_str())
        .collect();
    assert!(disagreeing.is_empty(), "disagreements in {disagreeing:?}");
}

#[test]
#[ignore = "requires the owner's RAW files: set LUXFORGE_RAW_OWNER_DIR"]
fn owner_raw_headers_are_bounded_and_equal_the_whole_file() {
    let Some(dir) = directory("LUXFORGE_RAW_OWNER_DIR") else {
        return;
    };
    let rows: Vec<Row> = ["nikon_z6.NEF", "fujifilm_x100vi.RAF", "mavic_air_2s.DNG"]
        .iter()
        .map(|name| check(&dir.join(name), None))
        .collect();
    report(&rows);
}

#[test]
#[ignore = "requires the private RAW corpus: set LUXFORGE_RAW_CORPUS_DIR and LUXFORGE_RAW_POPULAR_DIR"]
fn corpus_raw_headers_are_bounded_and_agree_with_kamadak_and_libraw() {
    let mut rows = Vec::new();
    for variable in ["LUXFORGE_RAW_CORPUS_DIR", "LUXFORGE_RAW_POPULAR_DIR"] {
        if let Some(dir) = directory(variable) {
            rows.extend(check_directory(&dir, &RAW_EXTENSIONS));
        }
    }
    report(&rows);
}

#[test]
#[ignore = "requires private camera JPEGs: set LUXFORGE_JPEG_DIR"]
fn private_jpeg_headers_are_bounded_and_agree_with_kamadak() {
    let Some(dir) = directory("LUXFORGE_JPEG_DIR") else {
        return;
    };
    report(&check_directory(&dir, &["jpg", "jpeg"]));
}
