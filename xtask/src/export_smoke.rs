//! The `export` smoke scenario: JPEG export from the real editor. It opens the EXIF orientation 6
//! fixture, brightens it by one stop and crops it to 16:9, so the output differs from the original
//! in pixels, in orientation and in size; opens the title bar's Export menu; exports the displayed
//! entry with metadata stripped and again keeping it, into the run's evidence directory through the
//! chain the menu starts (only the save dialog is bypassed); exports it through the reference
//! renderer, as the palette's Export reference render… does (`reference: true`); exports while the
//! comparison selects Before, checking identical edited JPEG bytes; and exports once more with Keep
//! metadata to the stripped file's name, which the core refuses without touching the file.
//!
//! The written files are checked independently of the editor: each is decoded here with the `image`
//! crate and must have the dimensions of the captured output stage and of the job's own record, the
//! byte length the job reported, an embedded ICC profile, and pixels that agree between the files
//! and are brighter than the original's. Their segments are read here too: the stripped file
//! carries no APP1 at all, and the kept one exactly one EXIF APP1 whose Orientation is 1 and no XMP.
//! The stripped file still has no APP1 and its reported length after the refused export, so the
//! refusal replaced nothing, and the fixture's hash is the one the run recorded before launching.
//!
//! The renderer (`docs/design/gpu-first.md`, stage 4): each export's result names the GPU, but the
//! reference one's, which names the reference as requested. The GPU's stripped export, decoded, is
//! within the pointwise display limit of the reference export decoded, over every pixel
//! (`luxforge_reference::preview_error`); the two GPU exports of the edited entry, stripped and
//! during comparison, are the same bytes; and the GPU tile worker drew them on the adapter that
//! draws the window, as each export's `export_finished` event records.
//!
//! The settings the desktop does not send yet, through the API (`api_exports`): once the editor
//! has quit, an owner of the runner's own over a copy of the run's catalog writes the edited entry
//! with the Web and Master presets as `export.presets.list` lists them and as an optimised WebP
//! resized to 160 px, on the reference renderer since it has no GPU. Each is decoded here with the
//! `image`, `tiff` and `image-webp` crates and must be the plan's output, the result's length and
//! format, carry the sRGB profile and an EXIF Orientation of 1, be brighter than the original and
//! have the structure its settings ask for: a progressive 4:2:0 JPEG with no density, a 16-bit
//! Deflate TIFF at 300 ppi, a lossy WebP with its ICC and EXIF chunks.
use crate::{
    scenario::{Checked, Checks, Frame, Plan, Run, Step, Tolerance, plan::only},
    *,
};
use luxforge_evidence as script;

/// Today's export settings for a run's own API export into `folder`: a full-size quality-90 JPEG
/// at 4:4:4 with no metadata, each batch file named `<stem>-edited.jpg`, as the desktop's
/// Select batch writes. A single export names its destination, inside `folder`.
pub(crate) fn export_settings(folder: &Path) -> Value {
    json!(luxforge_core::ExportSettings {
        folder: luxforge_core::ExportFolder::Path(folder.to_path_buf()),
        suffix: "-edited".into(),
        ..luxforge_core::ExportSettings::baseline(luxforge_core::MetadataLevel::None)
    })
}
use luxforge_reference::preview_error::{self, Class, Rgb8};

pub const FIXTURE: &str = "fixtures/s0/orientation-6.jpg";
const STRIPPED: &str = "export-stripped.jpg";
const KEPT: &str = "export-kept.jpg";
const REFERENCE: &str = "export-reference.jpg";
const COMPARED: &str = "export-compared.jpg";
/// The EXIF Orientation tag.
const ORIENTATION: u16 = 0x0112;
/// How much brighter than the original, in mean luminance codes, the one-stop export must read.
const BRIGHTER: f64 = 10.0;
/// How far the two exports' mean channel values may differ: the same render through the same
/// encoder, so nothing but zero is expected.
const SAME: f64 = 0.01;
/// The title bar's height with its rule, in logical points, below which the menu drops.
const TITLE_BAR: f64 = 44.0;
/// The band under the title bar the open menu covers, in logical points below the bar. Its
/// columns are the window's left half, where the file's identity, Open and Export sit.
const MENU_BAND: f64 = 60.0;
/// The mean per-channel change across that band that counts as a menu drawn over it.
const MENU_CHANGE: f64 = 1.0;

/// Export the edited entry normally and during comparison, then refuse an existing destination.
pub fn plan(_: &[PathBuf]) -> Plan {
    let steps = vec![
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
        Step::new("reference", script::Step::export_reference(REFERENCE)).commits(0),
        Step::new("compare", script::CompareStep::Tap).commits(0),
        Step::new("compared", script::Step::export(COMPARED, false)).commits(0),
        Step::new("compare-exited", script::CompareStep::Tap).commits(0),
        // Keep metadata to the stripped file's name: were anything replaced, that file would
        // gain an APP1. The status bar says why nothing was written.
        Step::new("refused", script::Step::export(STRIPPED, true))
            .commits(0)
            .refused("conflict")
            .status(format!(
                "Not exported: {STRIPPED} already exists; Luxforge never replaces a file"
            )),
    ];
    let native = |name: &str, action: &str| {
        Step::new(
            name,
            script::Step::WindowVisibility {
                action: action.into(),
            },
        )
        .commits(0)
    };
    let mut monitored = Vec::new();
    for step in steps {
        let name = step.name().to_owned();
        if cfg!(target_os = "macos") && matches!(name.as_str(), "stripped" | "compared" | "refused")
        {
            monitored.push(native(&format!("hidden-{name}"), "hide_window"));
        }
        monitored.push(step);
        if cfg!(target_os = "macos") {
            match name.as_str() {
                "opened" => monitored.push(native("native-visible", "show_window")),
                "kept" | "compared" | "refused" => {
                    monitored.push(native(&format!("restored-{name}"), "show_window"))
                }
                _ => {}
            }
        }
    }
    Plan::new(monitored)
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
    /// Its pixels as the decoder here reads them, three bytes each, row by row.
    rgb: Vec<u8>,
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
        rgb: image.into_raw(),
    })
}

/// The GPU tile worker's figures each export's `export_finished` event records in `step`.
fn worker(launch: &Checked, step: &str) -> Result<Value> {
    let events = crate::gpu_preview_smoke::step_events(launch, step)?;
    let finished = crate::gpu_preview_smoke::named(events, "export_finished");
    let [finished] = finished.as_slice() else {
        return Err(format!(
            "The {step} step logged {} export_finished events",
            finished.len()
        )
        .into());
    };
    Ok(finished["detail"]["tiles"].clone())
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

/// The mean per-channel difference between two captures over the band the menu drops into, in the
/// window's left half.
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
        for x in 0..a.width() / 2 {
            let (p, q) = (a.get_pixel(x, y), b.get_pixel(x, y));
            for c in 0..3 {
                total += (f64::from(p[c]) - f64::from(q[c])).abs();
            }
            count += 3.0;
        }
    }
    Ok(total / count.max(1.0))
}

pub fn verify(run: &mut Run, launches: &[Checked]) -> Result {
    let launch = only(launches)?;
    let root = run.root().to_owned();
    let mut checks = Checks::new();
    if cfg!(target_os = "macos") {
        for (hidden, outcome) in [
            ("hidden-stripped", "stripped"),
            ("hidden-stripped", "kept"),
            ("hidden-compared", "compared"),
            ("hidden-refused", "refused"),
        ] {
            let before = launch.at(hidden)?;
            let after = launch.at(outcome)?;
            ensure(
                after.state()["visibility"]["window_hidden"] == true
                    && after.state()["visibility"]["sampling_allowed"] == false
                    && after.state()["visibility"]["evidence_invisible_window_override"] == false,
                "An export outcome was not adopted under the native hidden gate",
            )?;
            ensure(
                before.state()["performance"]["reads_requested"]
                    == after.state()["performance"]["reads_requested"],
                "Hidden export work admitted resource sampling",
            )?;
            checks.note(
                after,
                "export outcome adopted while native hidden",
                after.state()["visibility"].clone(),
            );
        }
    }

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
    checks.compare(
        menu,
        "the band under the title bar with the Export menu open, against the frame before it",
        band_change(cropped, menu)?,
        0.0,
        Tolerance::Above(MENU_CHANGE),
    )?;

    // The original, decoded here, for the brightness comparison; its mean ignores orientation.
    let source = image::open(root.join(FIXTURE))?.to_rgb8();
    let original = mean_luminance(mean_rgb(&source));

    // Every export but the reference one is the GPU's; the reference one asked for the reference.
    let gpu = json!({"record": "gpu", "reason": null});
    let requested = json!({"record": "reference", "reason": "requested"});
    let mut written = Vec::new();
    for (step, file, keep, renderer) in [
        ("stripped", STRIPPED, false, &gpu),
        ("kept", KEPT, true, &gpu),
        ("reference", REFERENCE, false, &requested),
        ("compared", COMPARED, false, &gpu),
    ] {
        let frame = launch.at(step)?;
        let export = record(frame, step)?;
        let result = &export["record"]["result"];
        ensure(
            result["renderer"] == *renderer,
            format!(
                "The {step} export names {} as its renderer, not {renderer}",
                result["renderer"]
            ),
        )?;
        ensure(
            export["queued"]["reference"] == json!(renderer == &requested),
            format!(
                "The {step} export was accepted with reference {}",
                export["queued"]["reference"]
            ),
        )?;
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
                && export["queued"]["settings"]["metadata"]
                    == json!(if keep { "all" } else { "none" }),
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
        let status = frame.status()?;
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
        checks.compare(
            frame,
            &format!("{file}'s mean luminance against the original's"),
            mean_luminance(read.mean),
            original,
            Tolerance::Above(BRIGHTER),
        )?;
        checks.note(
            frame,
            "an export written and read back",
            json!({"file":file,"keep_metadata":keep,"decoded":[read.size.0,read.size.1],
            "bytes":read.bytes,"app1":read.app1.len(),"icc":read.icc,"metadata":result["metadata"],
            "renderer":result["renderer"]}),
        );
        written.push(read);
    }

    // The GPU's stripped export against the reference renderer's, both decoded here, over every
    // pixel, by the pointwise display limit: an exposure and a crop are colour steps.
    let reference = launch.at("reference")?;
    let (gpu_export, reference_export) = (&written[0], &written[2]);
    let (width, height) = reference_export.size;
    let statistics = preview_error::compare(
        Rgb8::new(width, height, &gpu_export.rgb)?,
        Rgb8::new(width, height, &reference_export.rgb)?,
        [0, 0, width, height],
    )?;
    let limits = Class::Pointwise.limits();
    let figures = json!({
        "pixels": statistics.pixels, "mean": statistics.mean,
        "worst_block": statistics.worst_block, "p99": statistics.p99,
        "mean_delta_l": statistics.mean_delta_l, "max": statistics.max,
    });
    ensure(
        preview_error::verdict(&statistics, Class::Pointwise).passed(),
        format!(
            "The GPU export is past the pointwise display limit of the reference export: {figures}"
        ),
    )?;
    checks.note(
        reference,
        "the GPU's export within the pointwise display limit of the reference export, both decoded here",
        json!({
            "files": [STRIPPED, REFERENCE],
            "statistics": figures,
            "limits": {"mean": limits.mean, "worst_block": limits.worst_block, "p99": limits.p99,
                "mean_delta_l_abs": limits.mean_delta_l},
        }),
    );

    // The GPU tile worker drew each GPU export on the adapter that draws the window, one stream
    // each, and the reference export streamed nothing. The photograph's catalog tiers stream
    // through the same worker before the first export, so the count is held from that export on.
    let backend = &launch.at("stripped")?["state"]["backend"];
    let mut streams: Option<u64> = None;
    for (step, streamed) in [
        ("stripped", true),
        ("kept", true),
        ("reference", false),
        ("compared", true),
    ] {
        let tiles = worker(launch, step)?;
        ensure(
            tiles["status"] == "gpu"
                && tiles["adapter"]["backend"] == backend["backend"]
                && tiles["adapter"]["adapter"] == backend["adapter"],
            format!(
                "The {step} export's tile worker is not the GPU's on the window's adapter {backend}: \
                 {tiles}"
            ),
        )?;
        let now = tiles["streams"].as_u64().unwrap_or_default();
        ensure(
            streams.map_or(now >= 1, |before| now == before + u64::from(streamed)),
            format!("The {step} export left the tile worker at {now} streams after {streams:?}"),
        )?;
        streams = Some(now);
        checks.note(
            launch.at(step)?,
            "the GPU tile worker's figures as the export ended",
            tiles,
        );
    }
    checks.compare(
        launch.at("kept")?,
        "the stripped and kept exports' largest mean channel difference",
        (0..3)
            .map(|c| (written[0].mean[c] - written[1].mean[c]).abs())
            .fold(0.0, f64::max),
        0.0,
        Tolerance::Within(SAME),
    )?;
    let compared = launch.at("compared")?;
    ensure(
        compared["state"]["comparison"]["after_entry"] == json!(entry)
            && compared["state"]["stack"]["displayed"]["entry"] != json!(entry),
        "The comparison must select Before while retaining the edited After entry",
    )?;
    ensure(
        fs::read(launch.evidence.join(COMPARED))? == fs::read(launch.evidence.join(STRIPPED))?,
        "Export during comparison differs from the edited entry's export",
    )?;
    checks.note(
        compared,
        "comparison exports the fixed After entry, with identical JPEG bytes: two GPU exports of \
         one entry in one run, the same bytes",
        json!({"entry_id": entry, "files": [STRIPPED, COMPARED], "renderer": gpu}),
    );

    // The refusal, whose reason the plan holds, and the stripped file exactly as the first export
    // left it.
    let after = read_export(&launch.evidence.join(STRIPPED))?;
    ensure(
        after.app1.is_empty() && after.bytes == written[0].bytes,
        "The refused export changed the stripped file",
    )?;

    // The settings the desktop does not send yet, through the API of an owner of the runner's own
    // over the run's catalog.
    let queued = &record(launch.at("stripped")?, "stripped")?["queued"];
    let api = api_exports(launch, queued, output, original, &mut checks)?;

    // The original was only read.
    run.sources_unchanged()?;
    checks.write(
        &launch.evidence,
        "export",
        json!({
            "fixture": {"path": FIXTURE, "sha256": hash(&root.join(FIXTURE))?, "unchanged": true},
            "output_stage": [output.0, output.1],
            "api_exports": api,
            "same_tolerance": SAME,
            "brighter_margin": BRIGHTER,
            "scope": "Each written JPEG decoded with the image crate and its segments read here: dimensions, byte length, ICC profile, APP1 and EXIF orientation, mean colour against the original and each other, and the GPU's export against the reference renderer's by the pointwise display limit (CIEDE2000, luxforge_reference::preview_error) over every pixel of one small fixture: functional evidence of the GPU export, which the release gate measures over the corpus. The Web and Master presets and a resized optimised WebP are written through the API by an owner of the runner's own over a copy of the run's catalog, on the reference renderer since it has no GPU, and decoded here with the image, tiff and image-webp crates",
        }),
    )
}

/// The long edge the API's WebP export is resized to, below the output stage's, so the reference
/// resamples it.
const WEBP_EDGE: u32 = 160;

/// What one API export is, as the decoders here read it.
struct Decoded {
    size: (u32, u32),
    icc: bool,
    /// IFD0's Orientation in the file's EXIF, `None` when it carries no EXIF.
    orientation: Option<u32>,
    mean: [f64; 3],
    /// What the format's own structure says: a JPEG's frame marker, sampling and JFIF density; a
    /// TIFF's bits, compression and resolution; a WebP's chunks.
    structure: Value,
}

/// The EXIF Orientation in a TIFF structure, as `kamadak-exif` reads it.
fn exif_orientation(tiff: &[u8]) -> Option<u32> {
    let exif = exif::Reader::new().read_raw(tiff.to_vec()).ok()?;
    exif.get_field(exif::Tag::Orientation, exif::In::PRIMARY)
        .and_then(|field| field.value.get_uint(0))
}

fn decode_api_jpeg(bytes: &[u8]) -> Result<Decoded> {
    use image::ImageDecoder;
    let found = segments(bytes)?;
    let frame = found
        .iter()
        .find(|(marker, _)| matches!(marker, 0xC0 | 0xC2))
        .ok_or("The JPEG has no baseline or progressive frame")?;
    let jfif = found
        .iter()
        .find(|(marker, _)| *marker == 0xE0)
        .ok_or("The JPEG has no JFIF header")?
        .1;
    let orientation = found
        .iter()
        .find(|(marker, payload)| *marker == 0xE1 && payload.starts_with(b"Exif\0\0"))
        .and_then(|(_, payload)| exif_orientation(&payload[6..]));
    let mut decoder = image::codecs::jpeg::JpegDecoder::new(std::io::Cursor::new(bytes))?;
    let icc = decoder.icc_profile()?.is_some();
    let image = image::DynamicImage::from_decoder(decoder)?.to_rgb8();
    Ok(Decoded {
        size: image.dimensions(),
        icc,
        orientation,
        mean: mean_rgb(&image),
        structure: json!({
            "progressive": frame.0 == 0xC2,
            "sampling": frame.1[7],
            "jfif_density": [jfif[7], u16::from_be_bytes([jfif[8], jfif[9]])],
        }),
    })
}

fn decode_api_tiff(bytes: &[u8]) -> Result<Decoded> {
    use tiff::{
        ColorType,
        decoder::{Decoder, DecodingResult, ifd::Value as Tag},
        tags::Tag as Name,
    };
    let mut decoder = Decoder::new(std::io::Cursor::new(bytes))?;
    let (width, height) = decoder.dimensions()?;
    let ColorType::RGB(bits) = decoder.colortype()? else {
        return Err("The TIFF is not RGB".into());
    };
    let compression = decoder.get_tag_u32(Name::Compression)?;
    let unit = decoder.get_tag_u32(Name::ResolutionUnit)?;
    let resolution = match decoder.get_tag(Name::XResolution)? {
        Tag::Rational(numerator, denominator) => f64::from(numerator) / f64::from(denominator),
        other => return Err(format!("The TIFF's XResolution is {other:?}").into()),
    };
    let icc = decoder.get_tag_u8_vec(Name::IccProfile).is_ok();
    let rgb: Vec<u8> = match decoder.read_image()? {
        DecodingResult::U8(samples) => samples,
        DecodingResult::U16(samples) => samples.iter().map(|sample| (sample >> 8) as u8).collect(),
        _ => return Err("The TIFF's samples are neither 8 nor 16 bits".into()),
    };
    let image = image::RgbImage::from_raw(width, height, rgb).ok_or("The TIFF is short")?;
    Ok(Decoded {
        size: (width, height),
        icc,
        orientation: exif_orientation(bytes),
        mean: mean_rgb(&image),
        structure: json!({"bits": bits, "compression": compression, "resolution_unit": unit, "resolution": resolution}),
    })
}

fn decode_api_webp(bytes: &[u8]) -> Result<Decoded> {
    let mut decoder = image_webp::WebPDecoder::new(std::io::Cursor::new(bytes))?;
    let (width, height) = decoder.dimensions();
    let icc = decoder.icc_profile()?.is_some();
    let exif = decoder.exif_metadata()?;
    let alpha = decoder.has_alpha();
    let mut pixels = vec![
        0;
        decoder
            .output_buffer_size()
            .ok_or("The WebP is too large")?
    ];
    decoder.read_image(&mut pixels)?;
    let rgb: Vec<u8> = if alpha {
        pixels
            .chunks_exact(4)
            .flat_map(|pixel| [pixel[0], pixel[1], pixel[2]])
            .collect()
    } else {
        pixels
    };
    let image = image::RgbImage::from_raw(width, height, rgb).ok_or("The WebP is short")?;
    let mut chunks = Vec::new();
    let mut at = 12;
    while at + 8 <= bytes.len() {
        let len = u32::from_le_bytes([bytes[at + 4], bytes[at + 5], bytes[at + 6], bytes[at + 7]]);
        chunks.push(String::from_utf8_lossy(&bytes[at..at + 4]).into_owned());
        at += 8 + len as usize + len as usize % 2;
    }
    Ok(Decoded {
        size: (width, height),
        icc,
        orientation: exif.as_deref().and_then(exif_orientation),
        mean: mean_rgb(&image),
        structure: json!({"chunks": chunks}),
    })
}

/// Write the Web and Master presets and a resized optimised WebP of the run's edited entry through
/// the API, as an independent client does, with an owner of the runner's own over a copy of the
/// run's catalog (the editor has quit; the copy keeps the run's own catalog as it left it), and
/// check each independently: the plan's output stage is the frame's, the file decodes to the
/// plan's output and is the result's length and format, carries the sRGB profile and an EXIF
/// whose Orientation is 1, is brighter than the original, and is the structure its settings ask
/// for. This owner has no GPU, so the reference renders each, as the result names.
fn api_exports(
    launch: &Checked,
    queued: &Value,
    stage: (u32, u32),
    original: f64,
    checks: &mut Checks,
) -> Result<Value> {
    use luxforge_testkit::client::{call, prepare, request_id, settle};
    let copy = launch.evidence.join("api-catalog");
    fs::create_dir_all(&copy)?;
    for suffix in ["", "-wal", "-shm"] {
        let from = launch.evidence.join(format!("catalog.sqlite{suffix}"));
        if from.exists() {
            fs::copy(&from, copy.join(format!("catalog.sqlite{suffix}")))?;
        }
    }
    let (owner, join) = luxforge_core::OwnerHandle::start(&copy.join("catalog.sqlite"))
        .map_err(|error| format!("the core cannot open the run's catalog: {error}"))?;
    let client = owner.register();
    let outcome = (|| -> Result<Value> {
        let (asset, entry) = (&queued["asset_id"], &queued["entry_id"]);
        prepare(&owner, client, asset)?;
        let presets = call(&owner, client, "export.presets.list", json!({}))?;
        let preset = |id: &str| -> Result<Value> {
            presets["presets"]
                .as_array()
                .and_then(|presets| presets.iter().find(|preset| preset["preset_id"] == id))
                .map(|preset| preset["settings"].clone())
                .ok_or_else(|| format!("export.presets.list lists no {id}: {presets}").into())
        };
        let webp = json!({
            "format": "webp", "size": {"fit": "long_edge", "pixels": WEBP_EDGE}, "quality": 80,
            "optimise": true, "limit_bytes": null, "sharpen": null, "metadata": "all",
            "pixels_per_inch": null, "folder": "original", "suffix": "-webp",
        });
        let frame = launch.at("refused")?;
        let mut written = Vec::new();
        for (file, settings) in [
            ("api-web.jpg", preset("luxforge.web")?),
            ("api-master.tif", preset("luxforge.master")?),
            ("api-small.webp", webp),
        ] {
            let plan = call(
                &owner,
                client,
                "export.plan",
                json!({"asset_id": asset, "entry_id": entry, "settings": settings}),
            )?;
            ensure(
                plan["stage"] == json!({"width": stage.0, "height": stage.1}),
                format!(
                    "{file}'s plan has stage {}, the frame {stage:?}",
                    plan["stage"]
                ),
            )?;
            let output = (
                plan["output"]["width"].as_u64().unwrap_or_default() as u32,
                plan["output"]["height"].as_u64().unwrap_or_default() as u32,
            );
            let destination = launch.evidence.join(file);
            let job = call(
                &owner,
                client,
                "export.write",
                json!({
                    "asset_id": asset, "entry_id": entry, "settings": settings,
                    "destination": destination,
                    "mutation": {"request_id": request_id(file), "actor": "export-smoke"},
                }),
            )?;
            let read = settle(&owner, client, &job["job_id"])?;
            ensure(
                read["status"] == "ready",
                format!("The API export of {file} did not end ready: {read}"),
            )?;
            let result = &read["result"];
            let bytes = fs::read(&destination)?;
            let decoded = match settings["format"].as_str() {
                Some("jpeg") => decode_api_jpeg(&bytes)?,
                Some("tiff") => decode_api_tiff(&bytes)?,
                _ => decode_api_webp(&bytes)?,
            };
            ensure(
                decoded.size == output
                    && result["width"] == output.0
                    && result["height"] == output.1,
                format!(
                    "{file} decodes to {:?}, the plan's output is {output:?}, the result names {} \u{d7} {}",
                    decoded.size, result["width"], result["height"]
                ),
            )?;
            ensure(
                result["bytes"] == bytes.len() && result["format"] == settings["format"],
                format!("{file} is {} bytes; the result is {result}", bytes.len()),
            )?;
            ensure(
                result["renderer"] == json!({"record": "reference", "reason": null}),
                format!("{file} names {} as its renderer", result["renderer"]),
            )?;
            ensure(decoded.icc, format!("{file} carries no ICC profile"))?;
            ensure(
                decoded.orientation == Some(1),
                format!("{file}'s EXIF orientation is {:?}", decoded.orientation),
            )?;
            let expected = match file {
                "api-web.jpg" => {
                    json!({"progressive": true, "sampling": 0x22, "jfif_density": [0, 1]})
                }
                "api-master.tif" => {
                    json!({"bits": 16, "compression": 8, "resolution_unit": 2, "resolution": 300.0})
                }
                _ => json!({"chunks": ["VP8X", "ICCP", "VP8 ", "EXIF"]}),
            };
            ensure(
                decoded.structure == expected,
                format!(
                    "{file}'s structure is {}, its settings ask for {expected}",
                    decoded.structure
                ),
            )?;
            checks.compare(
                frame,
                &format!("{file}'s mean luminance against the original's"),
                mean_luminance(decoded.mean),
                original,
                Tolerance::Above(BRIGHTER),
            )?;
            let record = json!({
                "file": file, "settings": settings, "output": [output.0, output.1],
                "bytes": bytes.len(), "quality": result["quality"], "metadata": result["metadata"],
                "structure": decoded.structure, "renderer": result["renderer"],
            });
            checks.note(
                frame,
                "an API export written and decoded here",
                record.clone(),
            );
            written.push(record);
        }
        Ok(Value::Array(written))
    })();
    owner.stop();
    let _ = join.join();
    outcome
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
