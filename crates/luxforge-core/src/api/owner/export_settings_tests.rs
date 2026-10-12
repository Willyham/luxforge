//! Export settings through `export.write`, as an independent JSON client sends them: every built-in
//! preset and each format, size mode, metadata level, Optimise and size limit writes a file whose
//! independent decode (`image`'s JPEG decoder, which is `zune-jpeg`; `image-webp`; the `tiff`
//! decoder, with the repository's own TIFF walker reading the metadata) is the plan's output and the
//! result's fields; progress rises through every phase, encoding pass and limit attempt; the
//! refusals the plan predicts are refused before anything renders; and nothing an export, a
//! refusal, a cancel or a limit search does touches the original, the history or another file.
use super::*;
use crate::export::{CaptureMetadata, encode::srgb_profile, presets};
use image::ImageDecoder;
use std::io::Cursor;

/// The camera JPEG ([`camera_jpeg`]) imported: Make, Model and Artist in its EXIF.
fn camera(harness: &Harness) -> Value {
    let original = harness.dir.join("camera.jpg");
    fs::write(&original, camera_jpeg()).unwrap();
    harness.opened(&original)["asset"]["id"].clone()
}

/// What one written file is, as the independent decoders read it.
#[derive(Debug, PartialEq)]
struct Decoded {
    size: (u32, u32),
    /// The EXIF fields the repository's walker reads from the file's EXIF.
    metadata: Vec<&'static str>,
    icc: bool,
}

/// A JPEG's frame marker and its first component's sampling, and its JFIF density.
fn jpeg_structure(bytes: &[u8]) -> (u8, u8, (u8, u16, u16)) {
    let (marker, payload) = luxforge_jpeg::segments(bytes)
        .map(Result::unwrap)
        .find(|segment| matches!(segment.marker, 0xc0 | 0xc2))
        .map(|segment| (segment.marker, segment.payload))
        .expect("a baseline or progressive frame");
    (marker, payload[7], jfif_density(bytes))
}

fn decode_jpeg(bytes: &[u8]) -> Decoded {
    let mut decoder = image::codecs::jpeg::JpegDecoder::new(Cursor::new(bytes)).unwrap();
    let icc = decoder.icc_profile().unwrap();
    let size = decoder.dimensions();
    let mut pixels = vec![0; decoder.total_bytes() as usize];
    decoder.read_image(&mut pixels).expect("decodes");
    Decoded {
        size,
        metadata: CaptureMetadata::from_jpeg(bytes).field_names(),
        icc: icc.as_deref() == Some(srgb_profile()),
    }
}

/// A WebP's chunks after its header, `(fourcc, payload)`.
fn webp_chunks(file: &[u8]) -> Vec<([u8; 4], &[u8])> {
    let mut found = Vec::new();
    let mut at = 12;
    while at + 8 <= file.len() {
        let len = u32::from_le_bytes(file[at + 4..at + 8].try_into().unwrap()) as usize;
        found.push((
            file[at..at + 4].try_into().unwrap(),
            &file[at + 8..at + 8 + len],
        ));
        at += 8 + len + len % 2;
    }
    found
}

fn decode_webp(bytes: &[u8]) -> Decoded {
    let mut decoder = image_webp::WebPDecoder::new(Cursor::new(bytes)).unwrap();
    let size = decoder.dimensions();
    let mut pixels = vec![0; decoder.output_buffer_size().unwrap()];
    decoder.read_image(&mut pixels).expect("decodes");
    let icc = decoder.icc_profile().unwrap();
    let exif = webp_chunks(bytes)
        .into_iter()
        .find(|(fourcc, _)| fourcc == b"EXIF")
        .map(|(_, payload)| CaptureMetadata::from_raw(payload).field_names())
        .unwrap_or_default();
    Decoded {
        size,
        metadata: exif,
        icc: icc.as_deref() == Some(srgb_profile()),
    }
}

/// A TIFF's size, bits a sample, compression and density, as the `tiff` crate reads them.
fn tiff_structure(bytes: &[u8]) -> (u8, u32, (u32, tiff::decoder::ifd::Value)) {
    use tiff::{ColorType, decoder::Decoder, tags::Tag};
    let mut decoder = Decoder::new(Cursor::new(bytes)).unwrap();
    let ColorType::RGB(bits) = decoder.colortype().unwrap() else {
        panic!("not RGB");
    };
    let compression = decoder.get_tag_u32(Tag::Compression).unwrap();
    let unit = decoder.get_tag_u32(Tag::ResolutionUnit).unwrap();
    let resolution = decoder.get_tag(Tag::XResolution).unwrap();
    (bits, compression, (unit, resolution))
}

fn decode_tiff(bytes: &[u8]) -> Decoded {
    use tiff::{decoder::Decoder, tags::Tag};
    let mut decoder = Decoder::new(Cursor::new(bytes)).unwrap();
    let size = decoder.dimensions().unwrap();
    decoder.read_image().expect("decodes");
    let icc = decoder.get_tag_u8_vec(Tag::IccProfile).ok();
    Decoded {
        size,
        metadata: CaptureMetadata::from_raw(bytes).field_names(),
        icc: icc.as_deref() == Some(srgb_profile()),
    }
}

/// The fields each level keeps of the camera JPEG's Make, Model and Artist.
fn kept(level: &str) -> Vec<&'static str> {
    match level {
        "none" => vec![],
        "copyright" => vec!["Artist"],
        _ => vec!["Make", "Model", "Artist"],
    }
}

/// Every built-in preset, and each format, size mode, metadata level, Optimise and size limit,
/// through `export.plan` and `export.write` as an independent JSON client sends them: each file
/// decodes to the plan's output, is the result's length, names the result's format, quality and
/// fields, carries the sRGB profile and the settings' density, and is baseline or progressive,
/// 4:4:4 or 4:2:0, 8 or 16 bits and compressed or not as they say. A size-limited file is at most
/// its limit; a size-limited JPEG without metadata is byte for byte the reference frame encoded
/// at the quality the result names. Nothing else is written and the original is unchanged.
#[test]
#[slow]
fn every_preset_format_size_level_optimise_and_limit_writes_what_its_plan_says() {
    let mut harness = Harness::start("every-setting");
    let asset = camera(&harness);
    let original = harness.dir.join("camera.jpg");
    let original_digest = digest(&original);
    let entry = harness.expose(&asset, 0.4);
    let history = harness.ok("history.list", json!({"asset_id": asset}));
    let out = destinations(&harness);
    let builtin = presets::builtin();
    let jpeg = |changes: Value| settings_with(changes);
    let webp = |changes: Value| {
        let mut settings = json!({
            "format": "webp", "size": {"fit": "full"}, "quality": 80, "optimise": false,
            "limit_bytes": null, "sharpen": null, "metadata": "none", "pixels_per_inch": null,
            "folder": "original", "suffix": "",
        });
        for (field, value) in changes.as_object().unwrap() {
            settings[field] = value.clone();
        }
        settings
    };
    let tiff = |changes: Value| {
        let mut settings = builtin[2].settings.to_json();
        for (field, value) in changes.as_object().unwrap() {
            settings[field] = value.clone();
        }
        settings
    };
    let cases = [
        ("web.jpg", builtin[0].settings.to_json()),
        ("print.jpg", builtin[1].settings.to_json()),
        ("master.tif", builtin[2].settings.to_json()),
        (
            "within.jpg",
            jpeg(
                json!({"size": {"fit": "within", "width": 300, "height": 300}, "metadata": "copyright", "chroma": "420"}),
            ),
        ),
        (
            "optimised.jpg",
            jpeg(
                json!({"size": {"fit": "long_edge", "pixels": 240}, "optimise": true, "metadata": "no_location"}),
            ),
        ),
        (
            "limited.jpg",
            jpeg(json!({"quality": 100, "limit_bytes": 20_000})),
        ),
        (
            "limited-optimised.jpg",
            jpeg(
                json!({"quality": 100, "optimise": true, "chroma": "420", "limit_bytes": 16_000, "metadata": "all"}),
            ),
        ),
        (
            "full.webp",
            webp(json!({"metadata": "all", "pixels_per_inch": 72})),
        ),
        (
            "small.webp",
            webp(
                json!({"size": {"fit": "long_edge", "pixels": 200}, "optimise": true, "quality": 95, "metadata": "copyright"}),
            ),
        ),
        (
            "limited.webp",
            webp(json!({"quality": 100, "limit_bytes": 12_000, "optimise": true})),
        ),
        (
            "eight.tif",
            tiff(
                json!({"size": {"fit": "within", "width": 100, "height": 100}, "bit_depth": 8, "optimise": false, "metadata": "none", "pixels_per_inch": null}),
            ),
        ),
    ];
    let mut limited_quality = None;
    for (name, settings) in &cases {
        let planned = harness.ok(
            "export.plan",
            json!({"asset_id": asset, "settings": settings}),
        );
        assert_eq!(planned["notices"], json!([]), "{name}: {planned}");
        let output = (
            planned["output"]["width"].as_u64().unwrap() as u32,
            planned["output"]["height"].as_u64().unwrap() as u32,
        );
        let destination = out.join(name);
        let accepted = harness
            .export(json!({"asset_id": asset, "destination": destination, "settings": settings}));
        assert_eq!(accepted["settings"], *settings, "{name}");
        let read = harness.settle(&accepted["job_id"]);
        assert_eq!(read["status"], "ready", "{name}: {read}");
        let result = &read["result"];
        let bytes = fs::read(&destination).unwrap();
        let format = settings["format"].as_str().unwrap();
        let level = settings["metadata"].as_str().unwrap();
        assert_eq!(result["path"], json!(destination), "{name}");
        assert_eq!(result["bytes"], json!(bytes.len()), "{name}");
        assert_eq!(
            (result["width"].clone(), result["height"].clone()),
            (json!(output.0), json!(output.1)),
            "{name}"
        );
        assert_eq!(result["format"], format, "{name}");
        assert_eq!(result["optimised"], settings["optimise"], "{name}");
        assert_eq!(result["metadata"], json!(kept(level)), "{name}");
        assert_eq!(result["renderer"], reference_renderer(), "{name}");
        let limit = settings["limit_bytes"].as_u64();
        match (format, limit) {
            ("tiff", _) => assert!(result.get("quality").is_none(), "{name}"),
            (_, None) => assert_eq!(result["quality"], settings["quality"], "{name}"),
            (_, Some(limit)) => {
                let quality = result["quality"].as_u64().unwrap();
                assert!(
                    quality < settings["quality"].as_u64().unwrap() && quality >= 10,
                    "{name}: a search from 100 under {limit} chose {quality}"
                );
                assert!(bytes.len() as u64 <= limit, "{name}: {} bytes", bytes.len());
                if *name == "limited.jpg" {
                    limited_quality = Some(quality as u8);
                }
            }
        }
        let decoded = match format {
            "jpeg" => decode_jpeg(&bytes),
            "webp" => decode_webp(&bytes),
            _ => decode_tiff(&bytes),
        };
        assert_eq!(
            decoded,
            Decoded {
                size: output,
                metadata: kept(level),
                icc: true,
            },
            "{name}"
        );
        let density = settings["pixels_per_inch"].as_u64();
        match format {
            "jpeg" => {
                let (marker, sampling, jfif) = jpeg_structure(&bytes);
                let optimised = settings["optimise"] == json!(true);
                assert_eq!(marker, if optimised { 0xc2 } else { 0xc0 }, "{name}");
                let half = settings["chroma"] == json!("420");
                assert_eq!(sampling, if half { 0x22 } else { 0x11 }, "{name}");
                let expected = match density {
                    Some(ppi) => (1, ppi as u16, ppi as u16),
                    None => (0, 1, 1),
                };
                assert_eq!(jfif, expected, "{name}");
            }
            "webp" => {
                let chunks: Vec<[u8; 4]> = webp_chunks(&bytes)
                    .into_iter()
                    .map(|(fourcc, _)| fourcc)
                    .collect();
                assert!(chunks.contains(b"VP8 "), "{name}: lossy: {chunks:?}");
            }
            _ => {
                let (bits, compression, (unit, resolution)) = tiff_structure(&bytes);
                assert_eq!(u64::from(bits), settings["bit_depth"].as_u64().unwrap());
                let compressed = settings["optimise"] == json!(true);
                assert_eq!(compression, if compressed { 8 } else { 1 }, "{name}");
                let expected = match density {
                    Some(ppi) => (2, tiff::decoder::ifd::Value::Rational(ppi as u32, 1)),
                    None => (1, tiff::decoder::ifd::Value::Rational(1, 1)),
                };
                assert_eq!((unit, resolution), expected, "{name}");
            }
        }
    }
    let mut written: Vec<String> = cases.iter().map(|(name, _)| (*name).to_owned()).collect();
    written.sort();
    assert_eq!(listing(&out), written, "nothing else written");
    assert_eq!(digest(&original), original_digest, "the original");
    assert_eq!(
        harness.ok("history.list", json!({"asset_id": asset})),
        history,
        "exports change no history"
    );
    harness.stop();
    // The limited JPEG is the reference frame encoded once at the quality the search chose.
    let frame = reference(&harness.catalog, &asset, &entry);
    let mut expected = Vec::new();
    crate::export::encode::encode_jpeg(
        &mut expected,
        &frame,
        &crate::export::encode::JpegOptions {
            quality: limited_quality.unwrap(),
            ..crate::export::encode::BASELINE_90
        },
        None,
        None,
        &mut |_| {},
        &|| Ok(()),
    )
    .unwrap();
    assert!(
        fs::read(out.join("limited.jpg")).unwrap() == expected,
        "the limited file is the frame at the quality named"
    );
}

/// Each `job.read` of a running export, taken on the export lane each time its progress changes
/// ([`Hold`]'s `"progress"`), for the export `export.write` accepts next.
struct Reads {
    job: Arc<Mutex<Option<Value>>>,
    reads: Arc<Mutex<Vec<Value>>>,
}

impl Reads {
    fn start(harness: &Harness) -> Self {
        let job: Arc<Mutex<Option<Value>>> = Arc::default();
        let reads: Arc<Mutex<Vec<Value>>> = Arc::default();
        let (owner, client) = (harness.owner.clone(), harness.client);
        let (named, taken) = (Arc::clone(&job), Arc::clone(&reads));
        harness.owner.hold_exports(Some(Arc::new(move |at| {
            if at != "progress" {
                return;
            }
            // The export's first progress can come before `export.write` has answered its id.
            let id =
                luxforge_testbase::wait_for("the export's job", || named.lock().unwrap().clone());
            let read = owner
                .call(
                    client,
                    ApiRequest {
                        id: "progress".into(),
                        method: "job.read".into(),
                        params: json!({"job_id": id}),
                        token: None,
                    },
                )
                .unwrap()
                .result
                .unwrap();
            taken.lock().unwrap().push(read["progress"].clone());
        })));
        Self { job, reads }
    }

    /// Export `params`, and answer every progress `job.read` read while it ran, then its final
    /// read's.
    fn export(&self, harness: &Harness, params: Value) -> Vec<(f64, String)> {
        self.reads.lock().unwrap().clear();
        *self.job.lock().unwrap() = None;
        let accepted = harness.export(params);
        *self.job.lock().unwrap() = Some(accepted["job_id"].clone());
        let read = harness.settle(&accepted["job_id"]);
        assert_eq!(read["status"], "ready", "{read}");
        let mut reads = self.reads.lock().unwrap().clone();
        reads.push(read["progress"].clone());
        reads
            .into_iter()
            .map(|progress| {
                (
                    progress["fraction"].as_f64().expect("determinate progress"),
                    progress["message"].as_str().unwrap().to_owned(),
                )
            })
            .collect()
    }
}

/// `job.read`'s progress rises through every phase of an export, in whole percent, never going
/// back, and ends at 1: an optimised JPEG's rows and then its passes; a size limit's attempts, each
/// named (`limiting 2/8`); the reference's render, its resampling of a smaller output and the
/// encoding; a WebP; and the writing.
#[test]
#[slow]
fn job_read_progress_is_monotonic_through_every_phase_pass_and_attempt() {
    let harness = Harness::start("progress");
    let asset = camera(&harness);
    let out = destinations(&harness);
    let reads = Reads::start(&harness);
    let phases = |reads: &[(f64, String)]| -> Vec<String> {
        let mut phases: Vec<String> = Vec::new();
        for (_, message) in reads {
            let phase = message.split(' ').next().unwrap().to_owned();
            if phases.last() != Some(&phase) {
                phases.push(phase);
            }
        }
        phases
    };
    for (name, settings, expected) in [
        (
            "optimised.jpg",
            settings_with(json!({"optimise": true, "chroma": "420"})),
            &["rendering", "encoding", "optimising", "writing"][..],
        ),
        (
            "limited.jpg",
            settings_with(json!({"quality": 100, "optimise": true, "limit_bytes": 16_000})),
            &["rendering", "limiting", "writing"],
        ),
        (
            "resized.tif",
            presets::builtin()[2].settings.to_json().tap(|settings| {
                settings["size"] = json!({"fit": "long_edge", "pixels": 300});
            }),
            &["rendering", "resampling", "encoding", "writing"],
        ),
        (
            "frame.webp",
            json!({
                "format": "webp", "size": {"fit": "full"}, "quality": 80, "optimise": true,
                "limit_bytes": null, "sharpen": null, "metadata": "none", "pixels_per_inch": null,
                "folder": "original", "suffix": "",
            }),
            &["rendering", "encoding", "writing"],
        ),
    ] {
        let read = reads.export(
            &harness,
            json!({"asset_id": asset, "destination": out.join(name), "settings": settings}),
        );
        for pair in read.windows(2) {
            assert!(
                pair[1].0 >= pair[0].0,
                "{name}: progress went back: {read:?}"
            );
        }
        for (fraction, _) in &read {
            assert_eq!(
                (fraction * 100.0).round() / 100.0,
                *fraction,
                "{name}: whole percent"
            );
        }
        assert_eq!(
            read.last().unwrap(),
            &(1.0, "writing".to_owned()),
            "{name}: {read:?}"
        );
        assert_eq!(phases(&read), expected, "{name}: {read:?}");
        if name == "optimised.jpg" {
            let passes = read
                .iter()
                .filter(|(_, message)| message == "optimising")
                .count();
            assert!(passes > 3, "the passes report as they go: {read:?}");
        }
        if name == "limited.jpg" {
            let attempts: Vec<u32> = read
                .iter()
                .filter_map(|(_, message)| message.strip_prefix("limiting "))
                .map(|attempt| {
                    let (attempt, of) = attempt.split_once('/').unwrap();
                    assert_eq!(of, "8");
                    attempt.parse().unwrap()
                })
                .collect();
            assert!(
                attempts.windows(2).all(|pair| pair[1] >= pair[0]),
                "{attempts:?}"
            );
            assert!(
                *attempts.last().unwrap() > 1,
                "a search of several attempts: {read:?}"
            );
        }
    }
    harness.owner.hold_exports(None);
}

/// A value whose `edit` changed it.
trait Tap: Sized {
    fn tap(mut self, edit: impl FnOnce(&mut Self)) -> Self {
        edit(&mut self);
        self
    }
}

impl Tap for Value {}

/// A JPEG `width` × `height` of deterministic noise, which no quality compresses below the
/// smallest size limit.
fn noise_jpeg(path: &Path, width: u32, height: u32) {
    let mut state = 0x2545_f491_u32;
    let picture = image::RgbImage::from_fn(width, height, |_, _| {
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            (state & 0xff) as u8
        };
        image::Rgb([next(), next(), next()])
    });
    image::codecs::jpeg::JpegEncoder::new_with_quality(fs::File::create(path).unwrap(), 95)
        .encode_image(&picture)
        .unwrap();
}

/// What the plan predicts is refused by `export.write` before anything renders, with the plan's
/// code: a WebP wider than 16,383 px. A limit nothing fits fails the job `limit-unreachable`; a
/// cancel while an optimised JPEG runs its passes, and while a limit search makes its attempts,
/// ends the export cancelled. None of these writes a file or leaves a temporary one, and none
/// changes the original, its history or a file already there.
#[test]
#[slow]
fn refusals_failed_searches_and_cancels_write_nothing_and_change_nothing() {
    let harness = Harness::start("never-destructive");
    let wide = harness.dir.join("wide.jpg");
    image::codecs::jpeg::JpegEncoder::new_with_quality(fs::File::create(&wide).unwrap(), 90)
        // As wide as a source may be, one pixel wider than WebP can be.
        .encode_image(&image::RgbImage::from_pixel(
            16_384,
            8,
            image::Rgb([90, 120, 150]),
        ))
        .unwrap();
    let noisy = harness.dir.join("noisy.jpg");
    noise_jpeg(&noisy, 640, 480);
    let wide_asset = harness.opened(&wide)["asset"]["id"].clone();
    let noisy_asset = harness.opened(&noisy)["asset"]["id"].clone();
    let digests = [digest(&wide), digest(&noisy)];
    let histories = [&wide_asset, &noisy_asset]
        .map(|asset| harness.ok("history.list", json!({"asset_id": asset})));
    let out = destinations(&harness);
    let existing = out.join("existing.jpg");
    fs::write(&existing, b"keep me").unwrap();
    let webp = json!({
        "format": "webp", "size": {"fit": "full"}, "quality": 80, "optimise": false,
        "limit_bytes": null, "sharpen": null, "metadata": "none", "pixels_per_inch": null,
        "folder": "original", "suffix": "",
    });

    // Refused before anything renders, with the plan's code.
    let planned = harness.ok(
        "export.plan",
        json!({"asset_id": wide_asset, "settings": webp}),
    );
    assert_eq!(planned["notices"][0]["code"], "webp-too-large", "{planned}");
    assert_eq!(planned["notices"][0]["refuses"], json!(true));
    let refused = harness.refused(
        "export.write",
        with_envelope(
            json!({"asset_id": wide_asset, "destination": out.join("wide.webp"), "settings": webp}),
        ),
    );
    assert_eq!(refused.code, "validation", "{refused:?}");
    assert_eq!(refused.data.unwrap()["notice"], "webp-too-large");
    assert!(refused.job_id.is_none(), "nothing queued");

    // A limit nothing fits: the search ends the job, writing nothing.
    for (name, settings) in [
        (
            "unreachable.jpg",
            settings_with(json!({"limit_bytes": 10_240})),
        ),
        ("unreachable.webp", {
            let mut settings = webp.clone();
            settings["limit_bytes"] = json!(10_240);
            settings
        }),
    ] {
        let job = harness.export(
            json!({"asset_id": noisy_asset, "destination": out.join(name), "settings": settings}),
        );
        let read = harness.settle(&job["job_id"]);
        assert_eq!(read["status"], "failed", "{name}: {read}");
        assert_eq!(read["error"]["code"], "validation", "{name}: {read}");
        assert_eq!(
            read["error"]["data"]["code"], "limit-unreachable",
            "{name}: {read}"
        );
        assert_eq!(listing(&out), ["existing.jpg"], "{name}");
    }

    // Cancelled while optimising (its temporary file staged) and while limiting (none staged).
    for (phase, settings, staged) in [
        ("optimising", settings_with(json!({"optimise": true})), 1),
        (
            "limiting",
            settings_with(json!({"quality": 100, "limit_bytes": 20_000})),
            0,
        ),
    ] {
        let (reached, release) = harness.hold_at(phase);
        let job = harness.export(json!({
            "asset_id": noisy_asset, "destination": out.join(format!("{phase}.jpg")), "settings": settings,
        }));
        reached.recv_timeout(luxforge_testbase::HANG).unwrap();
        assert_eq!(
            staged_files(&out).len(),
            staged,
            "{phase}: {:?}",
            listing(&out)
        );
        harness.ok("job.cancel", json!({"job_id": job["job_id"]}));
        release.send(()).unwrap();
        let read = harness.settle(&job["job_id"]);
        assert_eq!(read["status"], "cancelled", "{phase}: {read}");
        assert_eq!(listing(&out), ["existing.jpg"], "{phase}");
        harness.owner.hold_exports(None);
    }

    assert_eq!(fs::read(&existing).unwrap(), b"keep me");
    assert_eq!([digest(&wide), digest(&noisy)], digests, "the originals");
    for (asset, history) in [&wide_asset, &noisy_asset].into_iter().zip(&histories) {
        assert_eq!(
            harness.ok("history.list", json!({"asset_id": asset})),
            *history,
            "no history changed"
        );
    }
}
