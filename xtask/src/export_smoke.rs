//! The `export` smoke scenario: JPEG export from the real editor. It opens the EXIF orientation 6
//! fixture, brightens it by one stop and crops it to 16:9, so the output differs from the original
//! in pixels, in orientation and in size; opens the title bar's Export menu; exports the displayed
//! entry with metadata stripped and again keeping it, into the run's evidence directory through the
//! chain the menu starts (only the save dialog is bypassed); and exports once more with Keep
//! metadata to the stripped file's name, which the core refuses without touching the file.
//!
//! The written files are checked independently of the editor: each is decoded here with the `image`
//! crate and must have the dimensions of the captured output stage and of the job's own record, the
//! byte length the job reported, an embedded ICC profile, and pixels that agree between the two
//! files and are brighter than the original's. Their segments are read here too: the stripped file
//! carries no APP1 at all, and the kept one exactly one EXIF APP1 whose Orientation is 1 and no XMP.
//! The stripped file still has no APP1 and its reported length after the refused export, so the
//! refusal replaced nothing, and the fixture's hash is the one the run recorded before launching.
use crate::{
    scenario::{Checked, Frame, Plan, Run, Step, plan::only},
    *,
};
use luxforge_evidence as script;

pub const FIXTURE: &str = "fixtures/s0/orientation-6.jpg";
const STRIPPED: &str = "export-stripped.jpg";
const KEPT: &str = "export-kept.jpg";
/// The EXIF Orientation tag.
const ORIENTATION: u16 = 0x0112;
/// How much brighter than the original, in mean luminance codes, the one-stop export must read.
const BRIGHTER: f64 = 10.0;
/// How far the two exports' mean channel values may differ: the same render through the same
/// encoder, so nothing but zero is expected.
const SAME: f64 = 0.01;
/// The title bar's height with its rule, in logical points, below which the menu drops.
const TITLE_BAR: f64 = 44.0;
/// The band under the title bar the open menu covers, in logical points below the bar.
const MENU_BAND: f64 = 60.0;
/// The mean per-channel change across that band that counts as a menu drawn over it.
const MENU_CHANGE: f64 = 1.0;

/// Every frame, in order: the open, two edits, the menu, two exports and a refused one.
pub fn plan(_: &[PathBuf]) -> Plan {
    Plan::new(vec![
        Step::opened("opened"),
        Step::new(
            "exposure",
            script::Step::call("edit.set-basic", json!({"exposure":1.0})),
        )
        .commits(1)
        .label("Exposure +1.00 EV"),
        Step::new(
            "cropped",
            script::Step::call("edit.crop-fit", json!({"aspect":"16:9","angle":0.0})),
        )
        .commits(1)
        .label("Crop 16:9"),
        Step::new("menu", script::Step::export_menu()).commits(0),
        Step::new("stripped", script::Step::export(STRIPPED, false)).commits(0),
        Step::new("kept", script::Step::export(KEPT, true)).commits(0),
        // Keep metadata to the stripped file's name: were anything replaced, that file would
        // gain an APP1.
        Step::new("refused", script::Step::export(STRIPPED, true))
            .commits(0)
            .refused("conflict"),
    ])
}

/// One JPEG's marker segments before its scan, as (marker, payload).
fn segments(bytes: &[u8]) -> Result<Vec<(u8, &[u8])>> {
    ensure(
        bytes.starts_with(&[0xFF, 0xD8]),
        "The export does not start with SOI",
    )?;
    let mut segments = Vec::new();
    let mut at = 2;
    while at + 4 <= bytes.len() {
        ensure(bytes[at] == 0xFF, format!("No marker at byte {at}"))?;
        let marker = bytes[at + 1];
        if marker == 0xDA {
            return Ok(segments);
        }
        let length = usize::from(u16::from_be_bytes([bytes[at + 2], bytes[at + 3]]));
        ensure(
            length >= 2 && at + 2 + length <= bytes.len(),
            format!("Segment {marker:02X} overruns the file"),
        )?;
        segments.push((marker, &bytes[at + 4..at + 2 + length]));
        at += 2 + length;
    }
    Err("The export has no scan".into())
}

/// IFD0's Orientation in an `Exif\0\0` APP1 payload.
fn orientation(app1: &[u8]) -> Result<Option<u16>> {
    let tiff = app1
        .strip_prefix(b"Exif\0\0")
        .ok_or("The APP1 is not EXIF")?;
    ensure(tiff.len() >= 8, "The EXIF header is truncated")?;
    let big = match &tiff[..2] {
        b"MM" => true,
        b"II" => false,
        _ => return Err("The EXIF has no byte order".into()),
    };
    let u16_at = |at: usize| -> Result<u16> {
        let pair = tiff.get(at..at + 2).ok_or("EXIF truncated")?;
        Ok(if big {
            u16::from_be_bytes([pair[0], pair[1]])
        } else {
            u16::from_le_bytes([pair[0], pair[1]])
        })
    };
    let u32_at = |at: usize| -> Result<u32> {
        let quad = tiff.get(at..at + 4).ok_or("EXIF truncated")?;
        let quad = [quad[0], quad[1], quad[2], quad[3]];
        Ok(if big {
            u32::from_be_bytes(quad)
        } else {
            u32::from_le_bytes(quad)
        })
    };
    ensure(u16_at(2)? == 42, "The EXIF is not TIFF")?;
    let ifd = u32_at(4)? as usize;
    let count = usize::from(u16_at(ifd)?);
    for entry in 0..count {
        let at = ifd + 2 + entry * 12;
        if u16_at(at)? == ORIENTATION {
            return Ok(Some(u16_at(at + 8)?));
        }
    }
    Ok(None)
}

/// What one written file is, read independently of the editor.
struct Written {
    bytes: u64,
    size: (u32, u32),
    app1: Vec<Vec<u8>>,
    icc: bool,
    mean: [f64; 3],
}

fn read_export(path: &Path) -> Result<Written> {
    let bytes = fs::read(path)?;
    let found = segments(&bytes)?;
    let app1 = found
        .iter()
        .filter(|(marker, _)| *marker == 0xE1)
        .map(|(_, payload)| payload.to_vec())
        .collect();
    let icc = found
        .iter()
        .any(|(marker, payload)| *marker == 0xE2 && payload.starts_with(b"ICC_PROFILE\0"));
    let image = image::load_from_memory_with_format(&bytes, image::ImageFormat::Jpeg)?.to_rgb8();
    Ok(Written {
        bytes: bytes.len() as u64,
        size: image.dimensions(),
        app1,
        icc,
        mean: mean_rgb(&image),
    })
}

fn mean_rgb(image: &image::RgbImage) -> [f64; 3] {
    let mut sum = [0.0; 3];
    for pixel in image.pixels() {
        for (channel, total) in sum.iter_mut().enumerate() {
            *total += f64::from(pixel[channel]);
        }
    }
    let count = f64::from(image.width()) * f64::from(image.height());
    sum.map(|total| total / count)
}

fn mean_luminance(rgb: [f64; 3]) -> f64 {
    0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2]
}

/// The output stage the frame shows: the displayed entry's exact dimensions.
fn stage(frame: &Frame) -> Result<(u32, u32)> {
    let dimensions = &frame["state"]["stack"]["displayed"]["dimensions"];
    let side = |index: usize| -> Result<u32> {
        dimensions[index]
            .as_u64()
            .and_then(|value| u32::try_from(value).ok())
            .ok_or_else(|| format!("The frame records no output stage: {dimensions}").into())
    };
    Ok((side(0)?, side(1)?))
}

/// The export's own record on the step, which must have ended `ready`.
fn record(frame: &Frame, step: &str) -> Result<Value> {
    let export = &frame["step"]["export"];
    ensure(
        export["record"]["status"] == "ready",
        format!("The {step} export did not end ready: {export}"),
    )?;
    Ok(export.clone())
}

/// The mean per-channel difference between two captures over the band the menu drops into.
fn band_change(before: &Frame, after: &Frame) -> Result<f64> {
    let (a, b) = (before.image()?, after.image()?);
    ensure(
        a.dimensions() == b.dimensions(),
        "The menu frame and the one before it differ in size",
    )?;
    let scale = after["scale"].as_f64().unwrap_or(1.0);
    let top = (TITLE_BAR * scale).round() as u32;
    let bottom = (((TITLE_BAR + MENU_BAND) * scale).round() as u32).min(a.height());
    let mut total = 0.0;
    let mut count: f64 = 0.0;
    for y in top..bottom {
        for x in 0..a.width() {
            let (p, q) = (a.get_pixel(x, y), b.get_pixel(x, y));
            for c in 0..3 {
                total += (f64::from(p[c]) - f64::from(q[c])).abs();
            }
            count += 3.0;
        }
    }
    Ok(total / count.max(1.0))
}

fn status_of(frame: &Frame) -> &str {
    frame["state"]["status"].as_str().unwrap_or_default()
}

pub fn verify(run: &mut Run, launches: &[Checked]) -> Result {
    let launch = only(launches)?;
    let root = run.root().to_owned();
    let mut checks = Vec::new();

    // The two edits: a brighter photograph whose output is 16:9 in the rotated orientation.
    let cropped = launch.at("cropped")?;
    let output = stage(cropped)?;
    let entry = cropped.entry()?.to_owned();
    ensure(
        output.0 > output.1,
        format!("The 16:9 crop's output stage is {output:?}"),
    )?;

    // The menu, open under its button and drawn over the band below the title bar.
    let menu = launch.at("menu")?;
    ensure(
        menu["state"]["export"]["menu_open"] == json!(true),
        format!("The Export menu is not open: {}", menu["state"]["export"]),
    )?;
    let change = band_change(cropped, menu)?;
    ensure(
        change > MENU_CHANGE,
        format!("Nothing was drawn under the title bar for the menu: {change:.2}"),
    )?;
    checks.push(json!({"frame":menu["file"],"shows":"the Export menu open under the title bar's Export button","band_change":change}));

    // The original, decoded here, for the brightness comparison; its mean ignores orientation.
    let source = image::open(root.join(FIXTURE))?.to_rgb8();
    let original = mean_luminance(mean_rgb(&source));

    let mut written = Vec::new();
    for (step, file, keep) in [("stripped", STRIPPED, false), ("kept", KEPT, true)] {
        let frame = launch.at(step)?;
        let export = record(frame, step)?;
        let result = &export["record"]["result"];
        let (width, height, bytes) = (
            result["width"].as_u64().unwrap_or_default(),
            result["height"].as_u64().unwrap_or_default(),
            result["bytes"].as_u64().unwrap_or_default(),
        );
        ensure(
            (width, height) == (u64::from(output.0), u64::from(output.1)),
            format!("The {step} job wrote {width} × {height}, the output stage is {output:?}"),
        )?;
        ensure(
            export["queued"]["entry_id"] == json!(entry)
                && export["queued"]["keep_metadata"] == json!(keep),
            format!(
                "The {step} job is not the displayed entry's: {}",
                export["queued"]
            ),
        )?;
        ensure(
            result["path"]
                .as_str()
                .is_some_and(|path| path.ends_with(file)),
            format!("The {step} job wrote {}", result["path"]),
        )?;
        let path = launch.evidence.join(file);
        let read = read_export(&path)?;
        ensure(
            read.size == output,
            format!(
                "{file} decodes to {:?}, the output stage is {output:?}",
                read.size
            ),
        )?;
        ensure(
            read.bytes == bytes,
            format!("{file} is {} bytes, the job reported {bytes}", read.bytes),
        )?;
        ensure(read.icc, format!("{file} embeds no ICC profile"))?;
        let status = status_of(frame);
        let prefix = format!("Exported {file} \u{b7} {width} \u{d7} {height} \u{b7} ");
        ensure(
            status.starts_with(&prefix) && (status.ends_with(" MB") || status.ends_with(" KB")),
            format!("The {step} status reads {status:?}"),
        )?;
        if keep {
            ensure(
                read.app1.len() == 1,
                format!(
                    "{file} has {} APP1 segments, expected one EXIF",
                    read.app1.len()
                ),
            )?;
            ensure(
                orientation(&read.app1[0])? == Some(1),
                format!("{file}'s EXIF orientation is not 1"),
            )?;
        } else {
            ensure(
                read.app1.is_empty(),
                format!("{file} carries {} APP1 segments", read.app1.len()),
            )?;
        }
        let brightness = mean_luminance(read.mean);
        ensure(
            brightness > original + BRIGHTER,
            format!(
                "{file} is not brighter than the original: {brightness:.1} against {original:.1}"
            ),
        )?;
        checks.push(
            json!({"frame":frame["file"],"file":file,"keep_metadata":keep,"status":status,
            "decoded":[read.size.0,read.size.1],"bytes":read.bytes,"app1":read.app1.len(),
            "icc":read.icc,"mean_luminance":brightness,"original_luminance":original,
            "metadata":result["metadata"]}),
        );
        written.push(read);
    }
    let difference = (0..3)
        .map(|c| (written[0].mean[c] - written[1].mean[c]).abs())
        .fold(0.0, f64::max);
    ensure(
        difference <= SAME,
        format!("The stripped and kept exports differ in pixels: {difference:.3}"),
    )?;

    // The refusal: its reason, and the stripped file exactly as the first export left it.
    let refused = launch.at("refused")?;
    let status = status_of(refused);
    ensure(
        status
            == format!("Not exported: {STRIPPED} already exists; Luxforge never replaces a file"),
        format!("The refused export's status reads {status:?}"),
    )?;
    let after = read_export(&launch.evidence.join(STRIPPED))?;
    ensure(
        after.app1.is_empty() && after.bytes == written[0].bytes,
        "The refused export changed the stripped file",
    )?;
    checks.push(json!({"frame":refused["file"],"status":status,"stripped_bytes":after.bytes}));

    // The original was only read.
    run.sources_unchanged()?;
    let fixture_hash = hash(&root.join(FIXTURE))?;
    checks.push(json!({"fixture":FIXTURE,"sha256":fixture_hash,"unchanged":true}));

    write_json(
        &launch.evidence.join("export-checks.json"),
        &json!({
            "checks": checks,
            "output_stage": [output.0, output.1],
            "same_tolerance": SAME,
            "brighter_margin": BRIGHTER,
            "scope": "Each written JPEG decoded with the image crate and its segments read here: dimensions, byte length, ICC profile, APP1 and EXIF orientation, and mean colour against the original and each other. Not a colorimetric claim",
        }),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_segment_reader_finds_app1_and_the_orientation() {
        // IFD0 with one entry, Orientation = 1, little-endian.
        let mut tiff = b"II*\0\x08\0\0\0".to_vec();
        tiff.extend([1, 0]);
        tiff.extend([0x12, 0x01, 3, 0, 1, 0, 0, 0, 1, 0, 0, 0]);
        tiff.extend([0, 0, 0, 0]);
        let mut app1 = b"Exif\0\0".to_vec();
        app1.extend(&tiff);
        let mut jpeg = vec![0xFF, 0xD8, 0xFF, 0xE1];
        jpeg.extend(u16::try_from(app1.len() + 2).unwrap().to_be_bytes());
        jpeg.extend(&app1);
        jpeg.extend([0xFF, 0xDA, 0, 2]);
        let found = segments(&jpeg).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].0, 0xE1);
        assert_eq!(orientation(found[0].1).unwrap(), Some(1));
        assert!(segments(&[0xFF, 0xD8, 0xFF, 0xE1, 0, 40]).is_err());
    }
}
