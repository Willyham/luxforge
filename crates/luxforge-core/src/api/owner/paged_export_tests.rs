//! A paged derived photograph exported and analysed through the catalog owner on the reference
//! renderer's banded mode (`docs/design/panorama-merge.md`, "The reference on paged sources"):
//! `analysis.request` and `export.write` render its output in bands from windows of its level 0,
//! and their answers are the banded reference's of the same pixels generated in memory; and a
//! source above 128 MP exports a valid JPEG and analyses with no whole frame.
use super::*;
use crate::{
    Cancel, RenderContext,
    analysis::reduce_bands,
    artifacts::derived_source::ChunkRect,
    export::{
        encode::{JpegOptions, encode_jpeg_rows},
        presets,
        settings::{
            Chroma, ExportFolder, ExportSettings, FormatSettings, MetadataLevel, OutputSize,
        },
    },
    modules::Region,
    render::{
        bands::{BandSource, Banded},
        bands_output::output_bands,
    },
    tiles::{Pixels, Sharpen, SharpenAmount, SharpenMedium},
};
use luxforge_test_macros::slow;
use luxforge_testbase::paths::{jpeg as fixture, temp_dir};
use std::fs;

/// A generated scene, in level-0 pixels: a ramp across the frame from below zero to past diffuse
/// white under a texture that repeats every 1,024 pixels, with a bright band near the top for the
/// Dehaze light to find. Equal chunks share their stored objects, so a large source stores few.
fn scene(channel: usize, x: u32, y: u32, width: u32, height: u32) -> f32 {
    let (u, v) = (x as f32 / width as f32, y as f32 / height as f32);
    let (tx, ty) = (x % 1024, y % 1024);
    let texture = ((tx * 37 + ty * 11 + channel as u32 * 5) % 97) as f32 / 97.0 - 0.5;
    let haze = if v < 0.1 { 1.5 } else { 0.0 };
    [-0.1 + 2.4 * u, 0.2 + 1.2 * v, 0.6 - 0.5 * u][channel] + 0.3 * texture + haze
}

/// The scene read straight from [`scene`], as the paged source's windows must read it: the
/// expected answers' source, which shares nothing with the stored chunks.
struct Generated {
    width: u32,
    height: u32,
}

impl BandSource for Generated {
    fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    fn settings(&self) -> crate::LinearSettings {
        crate::LinearSettings::default()
    }

    fn window(&self, area: Region, cancel: &Cancel) -> Result<Vec<f32>, Error> {
        let plane = area.pixels() as usize;
        let mut planes = vec![0.0; 3 * plane];
        for (row, y) in (area.y0..area.y1()).enumerate() {
            cancel.check()?;
            for (column, x) in (area.x0..area.x1()).enumerate() {
                for channel in 0..3 {
                    planes[channel * plane + row * area.width as usize + column] =
                        scene(channel, x, y, self.width, self.height);
                }
            }
        }
        Ok(planes)
    }
}

/// A catalog in its own directory holding a JPEG and a paged derived photograph of [`scene`],
/// written chunk by chunk, prepared and adopted, and an owner over it.
struct Opened {
    owner: OwnerHandle,
    join: Option<JoinHandle<()>>,
    client: ClientId,
    dir: PathBuf,
    asset: Value,
    width: u32,
    height: u32,
}

impl Opened {
    fn new(name: &str, width: u32, height: u32) -> Self {
        let dir = temp_dir(&format!("paged-owner-{name}"))
            .canonicalize()
            .unwrap();
        let catalog = dir.join("catalog.sqlite");
        let anchor = dir.join("anchor.jpg");
        fs::copy(fixture(), &anchor).unwrap();
        let asset = {
            let mut service = EditorService::open(&catalog).unwrap();
            service.import(&anchor).unwrap();
            let published =
                service
                    .derived_source_writer()
                    .write_with(
                        width,
                        height,
                        None,
                        &std::sync::atomic::AtomicBool::new(false),
                        |rect: ChunkRect, chunk| {
                            let pixels = rect.pixels();
                            for channel in 0..3 {
                                for row in 0..rect.height {
                                    for column in 0..rect.width {
                                        chunk[channel * pixels
                                            + (row * rect.width + column) as usize] = scene(
                                            channel,
                                            rect.x + column,
                                            rect.y + row,
                                            width,
                                            height,
                                        );
                                    }
                                }
                            }
                            Ok(())
                        },
                    )
                    .unwrap();
            service
                .publish_derived_with(&published, "merge.panorama", |_| Ok(()))
                .unwrap()
        };
        let (owner, join) = OwnerHandle::start(&catalog).unwrap();
        let client = owner.register();
        let opened = Self {
            owner,
            join: Some(join),
            client,
            dir,
            asset: json!(asset),
            width,
            height,
        };
        let queued = opened.ok("source.prepare", json!({"asset_id": opened.asset}));
        let record = opened.settled(&queued["job_id"]);
        assert_eq!(record["status"], "ready", "{record}");
        opened.ok("job.adopt", json!({"job_id": queued["job_id"]}));
        opened
    }

    fn call(&self, method: &str, params: Value) -> ApiResponse {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        self.owner
            .call(
                self.client,
                ApiRequest {
                    id: format!("{method}-{}", NEXT.fetch_add(1, Ordering::Relaxed)),
                    method: method.into(),
                    params,
                    token: None,
                },
            )
            .expect("the owner answered")
    }

    fn ok(&self, method: &str, params: Value) -> Value {
        let response = self.call(method, params);
        assert!(response.error.is_none(), "{method}: {:?}", response.error);
        response.result.expect("a result")
    }

    /// Read a job until it leaves `queued` and `running`, as a client does. A job over a photograph
    /// above 128 megapixels evaluates the whole frame through the reference bands, minutes of CPU
    /// time on a three-core runner beside the other tests, so its hang bound is four of the usual.
    fn settled(&self, job: &Value) -> Value {
        let bound = if u64::from(self.width) * u64::from(self.height) > 128_000_000 {
            luxforge_testbase::HANG * 4
        } else {
            luxforge_testbase::HANG
        };
        luxforge_testbase::wait_for_within(bound, "the job to settle", || {
            let read = self.ok("job.read", json!({"job_id": job}));
            (!matches!(read["status"].as_str(), Some("queued" | "running"))).then_some(read)
        })
    }

    fn state(&self) -> Value {
        self.ok("asset.state", json!({"asset_id": self.asset}))
    }

    fn edit(&self, method: &str, mut params: Value) {
        let revision = self.state()["revision"].clone();
        params["asset_id"] = self.asset.clone();
        params["mutation"] = json!({
            "expected_revision": revision,
            "request_id": format!("{method}-{revision}"),
            "actor": "test",
        });
        self.ok(method, params);
    }

    /// The current entry's stack.
    fn recipe(&self) -> crate::Recipe {
        let entry: crate::HistoryEntry =
            serde_json::from_value(self.state()["current_entry"].clone()).unwrap();
        entry.snapshot.recipe
    }

    /// The histogram `analysis.request` answers for the current entry.
    fn analysed(&self) -> Value {
        let requested = self.ok(
            "analysis.request",
            json!({"asset_id": self.asset, "target": {"kind": "current"}}),
        );
        let analysed = self.settled(&requested["job_id"]);
        assert_eq!(analysed["status"], "ready", "{analysed}");
        analysed["result"].clone()
    }

    /// The current entry exported to `name` in the catalog's directory with today's settings (a
    /// full-size quality-90 JPEG): the file and the job's result.
    fn exported(&self, name: &str) -> (PathBuf, Value) {
        self.exported_with(name, crate::export::settings::baseline_json_in(&self.dir))
    }

    /// The current entry exported to `name` in the catalog's directory with `settings`: the file
    /// and the job's result.
    fn exported_with(&self, name: &str, settings: Value) -> (PathBuf, Value) {
        let destination = self.dir.join(name);
        let accepted = self.ok(
            "export.write",
            json!({"asset_id": self.asset, "destination": destination, "settings": settings,
                   "mutation": {"request_id": name, "actor": "test"}}),
        );
        let exported = self.settled(&accepted["job_id"]);
        assert_eq!(exported["status"], "ready", "{exported}");
        (destination, exported["result"].clone())
    }
}

impl Drop for Opened {
    fn drop(&mut self) {
        self.owner.stop();
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// A paged photograph wider than the camera limits, edited with colour, a masked layer, Presence
/// with Dehaze and Detail, analyses and exports through the owner on the banded reference: its
/// histogram and its file are the banded reference's of the scene generated in memory, the file
/// names the reference with no reason on an owner with no GPU provider, and nothing allocates its
/// whole frame.
#[test]
#[slow]
fn reference_bands_export_and_analyse_a_paged_photograph_through_the_owner() {
    let opened = Opened::new("journey", 16_500, 300);
    opened.edit(
        "edit.set-basic",
        json!({"exposure": -0.5, "contrast": 15.0, "highlights": -20.0}),
    );
    opened.edit(
        "mask.create-radial",
        json!({"x": 0.5, "y": 0.5, "radius_x": 0.3, "radius_y": 0.3, "angle": 0.0,
               "feather": 40.0}),
    );
    let mask = opened.state()["current_entry"]["snapshot"]["recipe"]["masks"][0]["id"].clone();
    opened.edit(
        "edit.set-basic",
        json!({"mask": mask, "exposure": 0.5, "saturation": -30.0}),
    );
    opened.edit(
        "edit.set-presence",
        json!({"clarity": 25.0, "dehaze": 30.0}),
    );
    opened.edit("edit.set-detail", json!({"sharpening": 40.0}));

    let registry = ModuleRegistry::builtin();
    let compiled = registry
        .compile(opened.width, opened.height, &opened.recipe())
        .unwrap();
    let context = RenderContext::new();
    let generated = Generated {
        width: opened.width,
        height: opened.height,
    };
    let banded = Banded::new(&generated, &compiled, &context, &Cancel::never()).unwrap();
    let stage = banded.stage();
    let expected =
        reduce_bands(stage.width, stage.height, banded.bands(), &Cancel::never()).unwrap();
    assert_eq!(opened.analysed(), serde_json::to_value(expected).unwrap());

    let (path, result) = opened.exported("panorama-edited.jpg");
    assert_eq!(
        (result["width"].clone(), result["height"].clone()),
        (json!(stage.width), json!(stage.height))
    );
    assert_eq!(
        result["renderer"],
        json!({"record": "reference", "reason": null})
    );
    let mut expected = Vec::new();
    crate::export::encode::encode_jpeg_rows(
        &mut expected,
        (stage.width, stage.height),
        banded.bands(),
        &crate::export::encode::BASELINE_90,
        None,
        None,
        &mut |_| {},
        &|| Ok(()),
    )
    .unwrap();
    assert!(
        fs::read(&path).unwrap() == expected,
        "the export is the encoding of the banded reference"
    );
}

/// A paged photograph above 128 MP and wider than 16,384, with Dehaze, whose light is taken from
/// a reduction coarser than 16 on this stage, and Clarity, exports a JPEG of its full size that an
/// independent decoder reads, and its analysis counts every output pixel. The process's peak
/// footprint is printed for the record; the bounds held are the bands' own (each within the
/// evaluated-frame limit, its windows within the band budget) and the source's chunk cache.
#[test]
#[slow]
fn reference_bands_export_and_analyse_a_paged_source_above_128_megapixels() {
    let (width, height) = (20_000, 6_600);
    assert!(u64::from(width) * u64::from(height) > 128_000_000);
    assert!(crate::modules::estimate_reduction(crate::modules::Stage { width, height }) > 16);
    let opened = Opened::new("large", width, height);
    opened.edit(
        "edit.set-basic",
        json!({"exposure": -0.3, "contrast": 10.0}),
    );
    opened.edit(
        "edit.set-presence",
        json!({"clarity": 20.0, "dehaze": 25.0}),
    );

    let report = opened.analysed();
    let pixels = u64::from(width) * u64::from(height);
    for channel in ["r", "g", "b"] {
        let counted: u64 = report[channel]
            .as_array()
            .unwrap()
            .iter()
            .map(|count| count.as_u64().unwrap())
            .sum();
        assert_eq!(counted, pixels, "{channel}");
    }
    assert_eq!(
        (report["width"].clone(), report["height"].clone()),
        (json!(width), json!(height))
    );

    let (path, result) = opened.exported("large.jpg");
    assert_eq!(result["renderer"]["record"], "reference");
    let peak = luxforge_process::Sampler::new()
        .read_opening_nothing()
        .memory
        .peak_bytes;
    eprintln!("reference_bands: peak process footprint after a {width}x{height} export: {peak:?}");

    let file = fs::File::open(&path).unwrap();
    let mut decoder = image::codecs::jpeg::JpegDecoder::new(std::io::BufReader::new(file)).unwrap();
    use image::ImageDecoder;
    assert_eq!(decoder.dimensions(), (width, height));
    decoder.set_limits(image::Limits::no_limits()).unwrap();
    let mut rgb = vec![0_u8; decoder.total_bytes() as usize];
    decoder.read_image(&mut rgb).unwrap();
    // The decoded frame is the scene's: brighter on the right than on the left, as the ramp is.
    let column_mean = |x: u32| {
        (0..height)
            .step_by(97)
            .map(|y| f64::from(rgb[((y * width + x) * 3) as usize]))
            .sum::<f64>()
    };
    assert!(column_mean(width - 10) > column_mean(10));
    drop(rgb);

    // The Web preset's JPEG of it: a 2,048-pixel long edge resampled and sharpened band by band,
    // never through a whole stage, whose wide terminal alone (six bytes a pixel) would pass the
    // whole-frame reference's 512 MiB evaluated-frame limit.
    assert!(pixels * 6 > crate::render::limits::MAX_FRAME_BYTES);
    let web = preset_in(presets::WEB, &opened.dir);
    let (path, result) = opened.exported_with("large-web.jpg", web.to_json());
    assert_eq!(result["renderer"]["record"], "reference");
    let peak = luxforge_process::Sampler::new()
        .read_opening_nothing()
        .memory
        .peak_bytes;
    eprintln!("reference_bands: peak process footprint after its Web export: {peak:?}");
    let decoder =
        image::codecs::jpeg::JpegDecoder::new(std::io::Cursor::new(fs::read(&path).unwrap()))
            .unwrap();
    let output = (
        2048,
        (2048.0 * f64::from(height) / f64::from(width)).round() as u32,
    );
    assert_eq!(decoder.dimensions(), output);
    assert_eq!(
        (result["width"].clone(), result["height"].clone()),
        (json!(output.0), json!(output.1))
    );
}

/// A built-in preset's settings (`luxforge.web` or `luxforge.master`), saving into `folder` with
/// no metadata, so the file holds only what the pixels decide.
fn preset_in(preset_id: &str, folder: &std::path::Path) -> ExportSettings {
    let preset = presets::builtin()
        .into_iter()
        .find(|preset| preset.preset_id == preset_id)
        .expect("a built-in preset");
    ExportSettings {
        folder: ExportFolder::Path(folder.to_path_buf()),
        metadata: MetadataLevel::None,
        ..preset.settings
    }
}

/// The export of a paged photograph with every kind of output — the Web preset (resized to a
/// 2,048-pixel long edge, sharpened for the screen, optimised at 4:2:0), the Master preset (a
/// full-size 16-bit TIFF), a resized 16-bit TIFF sharpened for Matte paper at 720 ppi (the widest
/// kernel) and a resized WebP (a held format) — through the owner, on an owner with no GPU
/// provider: each names the reference with no reason, and each file is the banded reference's
/// output of the scene generated in memory, encoded: the JPEG byte for byte, the TIFFs' decoded
/// codes exactly, the WebP at its size.
#[test]
#[slow]
fn reference_bands_export_a_paged_photograph_with_every_setting_through_the_owner() {
    let opened = Opened::new("settings", 16_500, 300);
    opened.edit(
        "edit.set-basic",
        json!({"exposure": -0.4, "contrast": 20.0, "shadows": 30.0}),
    );
    opened.edit(
        "edit.set-presence",
        json!({"clarity": 20.0, "dehaze": 25.0}),
    );
    let registry = ModuleRegistry::builtin();
    let compiled = registry
        .compile(opened.width, opened.height, &opened.recipe())
        .unwrap();
    let context = RenderContext::new();
    let generated = Generated {
        width: opened.width,
        height: opened.height,
    };
    let banded = Banded::new(&generated, &compiled, &context, &Cancel::never()).unwrap();
    let stage = banded.stage();
    let cancel = Cancel::never();
    let expected = |settings: &ExportSettings| {
        let output = settings.output_spec((stage.width, stage.height));
        (output, output_bands(&banded, output, &cancel).unwrap())
    };

    // Web: resized, sharpened and optimised, encoded as the bands arrive.
    let web = preset_in(presets::WEB, &opened.dir);
    let (path, result) = opened.exported_with("web.jpg", web.to_json());
    let (output, bands) = expected(&web);
    assert_eq!(
        (output.width, output.height),
        (2048, (2048.0 * 300.0 / 16_500.0_f64).round() as u32)
    );
    assert_eq!(
        (result["width"].clone(), result["height"].clone()),
        (json!(output.width), json!(output.height))
    );
    assert_eq!(
        result["renderer"],
        json!({"record": "reference", "reason": null})
    );
    let FormatSettings::Jpeg {
        quality, chroma, ..
    } = web.format
    else {
        panic!("the Web preset is a JPEG");
    };
    assert_eq!(chroma, Chroma::Half);
    let mut encoded = Vec::new();
    encode_jpeg_rows(
        &mut encoded,
        (output.width, output.height),
        bands.map(|band| {
            band.map(|(rows, pixels)| match pixels {
                Pixels::Eight(rgba) => (rows, rgba),
                Pixels::Sixteen(_) => panic!("an 8-bit output"),
            })
        }),
        &JpegOptions {
            quality,
            chroma,
            optimise: true,
        },
        None,
        None,
        &mut |_| {},
        &|| Ok(()),
    )
    .unwrap();
    assert!(
        fs::read(&path).unwrap() == encoded,
        "the Web export is the encoding of the banded reference's output"
    );

    // Master, and a resized TIFF sharpened with the widest kernel: 16-bit codes, decoded exactly.
    let master = preset_in(presets::MASTER, &opened.dir);
    let matte = ExportSettings {
        size: OutputSize::LongEdge { pixels: 6000 },
        sharpen: Some(Sharpen {
            medium: SharpenMedium::Matte,
            amount: SharpenAmount::High,
        }),
        pixels_per_inch: Some(720),
        ..master.clone()
    };
    for (name, settings) in [("master.tif", master), ("matte.tif", matte)] {
        let (path, result) = opened.exported_with(name, settings.to_json());
        assert_eq!(result["renderer"]["record"], "reference", "{name}");
        let (output, bands) = expected(&settings);
        let mut codes = Vec::new();
        for band in bands {
            let Pixels::Sixteen(rgba) = band.unwrap().1 else {
                panic!("{name}: a 16-bit output");
            };
            codes.extend(
                rgba.chunks_exact(4)
                    .flat_map(|pixel| [pixel[0], pixel[1], pixel[2]]),
            );
        }
        let mut decoder =
            tiff::decoder::Decoder::new(std::io::Cursor::new(fs::read(&path).unwrap())).unwrap();
        assert_eq!(
            decoder.dimensions().unwrap(),
            (output.width, output.height),
            "{name}"
        );
        let tiff::decoder::DecodingResult::U16(decoded) = decoder.read_image().unwrap() else {
            panic!("{name}: a 16-bit TIFF");
        };
        assert!(
            decoded == codes,
            "{name}: the banded reference's 16-bit output"
        );
    }

    // A held format: a WebP assembled from the banded reference's resized bands.
    let webp = ExportSettings {
        format: FormatSettings::Webp {
            quality: 80,
            limit_bytes: None,
        },
        ..preset_in(presets::WEB, &opened.dir)
    };
    let (path, result) = opened.exported_with("web.webp", webp.to_json());
    let (output, _) = expected(&webp);
    assert_eq!(result["renderer"]["record"], "reference");
    let mut decoder =
        image_webp::WebPDecoder::new(std::io::Cursor::new(fs::read(&path).unwrap())).unwrap();
    assert_eq!(decoder.dimensions(), (output.width, output.height));
    let mut rgb = vec![0; decoder.output_buffer_size().unwrap()];
    decoder.read_image(&mut rgb).unwrap();
}
