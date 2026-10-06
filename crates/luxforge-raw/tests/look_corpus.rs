//! The RAW looks study's corpus export (`docs/design/raw-looks.md`, "The Standard look"): each
//! authentic file's as-shot development and its largest embedded camera JPEG, downscaled side by
//! side, for the reference crate's look study to measure the cameras' own looks against.
//! It only reads the files; nothing is committed.
//!
//! ```sh
//! LUXFORGE_LOOK_DIRS=/path/to/selection:/path/to/popular-extra:/path/to/owner/raw \
//! LUXFORGE_LOOK_OUTPUT=target/look-corpus \
//!   cargo test --release -p luxforge-raw --locked --test look_corpus look_corpus_export \
//!   -- --ignored --nocapture
//! ```
//!
//! Per file `<stem>`: `<stem>.dev`, the as-shot development in linear sRGB (the development, the
//! camera matrix `rgb_cam` and the default crop, as a RAW photo's Original renders before its
//! terminal transform), in sensor orientation, box-downscaled to at most `LONG_EDGE` pixels:
//! the bytes `LFDEV1`, width and height as little-endian `u32`, then interleaved RGB as
//! little-endian `f32`; and `<stem>.preview.png`, the decoded preview resized to the same size,
//! also in sensor orientation (cameras store their previews unrotated). `index.json` lists each
//! file's camera, orientation, sizes and why a file was skipped.
use luxforge_raw::{
    EmbeddedImage, EmbeddedPreviews, MAX_EMBEDDED_IMAGE_BYTES, MAX_EMBEDDED_READ_BUDGET, RawLayout,
    RawSource,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    env,
    fs::{self, File},
    io::Write as _,
    path::PathBuf,
    sync::atomic::AtomicBool,
};

/// The long edge of every exported image: enough for histograms and a contact sheet.
const LONG_EDGE: u32 = 768;
/// A preview whose aspect differs from the development's by more than this is not compared:
/// it is another framing, not the same picture.
const ASPECT_TOLERANCE: f64 = 0.03;
/// A preview below this long edge is a thumbnail, not a picture to match.
const MIN_PREVIEW_EDGE: u32 = 640;

/// The camera-space development converted by `rgb_cam`, as the core's source preparation does,
/// cropped to the default crop and box-downscaled by the integer factor that brings its long edge
/// to at most `LONG_EDGE`.
fn develop(source: &RawSource, cancel: &AtomicBool) -> (u32, u32, Vec<f32>) {
    let metadata = source.metadata();
    let planar = source
        .develop(metadata.as_shot_gains, cancel)
        .expect("develop as shot");
    let (width, height) = (planar.width as usize, planar.height as usize);
    let n = width * height;
    let matrix = metadata.rgb_cam;
    let crop = metadata.default_crop;
    let long = crop.width.max(crop.height);
    let factor = long.div_ceil(LONG_EDGE).max(1) as usize;
    let (out_w, out_h) = (crop.width as usize / factor, crop.height as usize / factor);
    let mut out = vec![0f32; out_w * out_h * 3];
    let scale = 1.0 / (factor * factor) as f64;
    for oy in 0..out_h {
        for ox in 0..out_w {
            let mut sum = [0f64; 3];
            for dy in 0..factor {
                let y = crop.y as usize + oy * factor + dy;
                for dx in 0..factor {
                    let x = crop.x as usize + ox * factor + dx;
                    let i = y * width + x;
                    let camera = [planar.data[i], planar.data[n + i], planar.data[2 * n + i]];
                    for (channel, row) in sum.iter_mut().zip(matrix) {
                        *channel +=
                            f64::from(row[0] * camera[0] + row[1] * camera[1] + row[2] * camera[2]);
                    }
                }
            }
            let o = (oy * out_w + ox) * 3;
            for c in 0..3 {
                out[o + c] = (sum[c] * scale) as f32;
            }
        }
    }
    (out_w as u32, out_h as u32, out)
}

/// The largest embedded JPEG, cut after its last EOI marker, decoded to 8-bit RGB.
fn preview(bytes: &[u8], cancel: &AtomicBool) -> Result<image::RgbImage, String> {
    let mut previews = EmbeddedPreviews::open(bytes, MAX_EMBEDDED_READ_BUDGET, cancel)
        .map_err(|error| format!("open: {error}"))?;
    let largest = *previews
        .listing()
        .largest_jpeg()
        .ok_or_else(|| "no embedded JPEG".to_string())?;
    let EmbeddedImage::Jpeg(mut jpeg) = previews
        .extract(largest.index, MAX_EMBEDDED_IMAGE_BYTES, cancel)
        .map_err(|error| format!("extract: {error}"))?
    else {
        return Err("the largest JPEG extracted as a bitmap".into());
    };
    if let Some(end) = jpeg.windows(2).rposition(|pair| pair == [0xff, 0xd9]) {
        jpeg.truncate(end + 2);
    }
    let decoded = image::load_from_memory_with_format(&jpeg, image::ImageFormat::Jpeg)
        .map_err(|error| format!("decode: {error}"))?;
    Ok(decoded.to_rgb8())
}

#[test]
#[ignore = "reads authentic RAW files named by LUXFORGE_LOOK_DIRS; run explicitly for the look study"]
fn look_corpus_export() {
    let dirs = env::var("LUXFORGE_LOOK_DIRS").expect("LUXFORGE_LOOK_DIRS");
    let output = PathBuf::from(env::var("LUXFORGE_LOOK_OUTPUT").expect("LUXFORGE_LOOK_OUTPUT"));
    fs::create_dir_all(&output).unwrap();
    let mut files = BTreeSet::new();
    for dir in env::split_paths(&dirs) {
        for entry in fs::read_dir(&dir).expect("list corpus directory") {
            let path = entry.unwrap().path();
            let name = path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .to_ascii_lowercase();
            let data = ["json", "tsv", "txt", "md"]
                .iter()
                .any(|extension| name.ends_with(&format!(".{extension}")));
            if path.is_file() && !data && !name.starts_with('.') {
                files.insert(fs::canonicalize(&path).unwrap());
            }
        }
    }
    let cancel = AtomicBool::new(false);
    let mut records: Vec<Value> = Vec::new();
    for path in &files {
        let stem = path.file_stem().unwrap().to_string_lossy().into_owned();
        let mut record = json!({ "id": stem, "file": path.to_string_lossy() });
        let bytes = fs::read(path).unwrap();
        let skip = |record: &mut Value, reason: String| {
            eprintln!("skip {stem}: {reason}");
            record["skipped"] = json!(reason);
        };
        let preview = match preview(&bytes, &cancel) {
            Ok(preview) => preview,
            Err(reason) => {
                skip(&mut record, reason);
                records.push(record);
                continue;
            }
        };
        let source = match RawSource::decode(&bytes, &cancel) {
            Ok(source) => source,
            Err(error) => {
                skip(&mut record, format!("decode: {error}"));
                records.push(record);
                continue;
            }
        };
        let metadata = source.metadata();
        record["make"] = json!(metadata.make);
        record["model"] = json!(metadata.model);
        record["orientation"] = json!(metadata.exif_orientation);
        record["preview_source"] = json!([preview.width(), preview.height()]);
        if metadata.layout == RawLayout::Monochrome {
            skip(&mut record, "monochrome sensor".into());
            records.push(record);
            continue;
        }
        if preview.width().max(preview.height()) < MIN_PREVIEW_EDGE {
            skip(&mut record, "preview below the minimum edge".into());
            records.push(record);
            continue;
        }
        let (width, height, development) = develop(&source, &cancel);
        let aspect = |w: u32, h: u32| f64::from(w.max(h)) / f64::from(w.min(h));
        let preview_landscape = preview.width() >= preview.height();
        let development_landscape = width >= height;
        let aspect_gap =
            (aspect(width, height) / aspect(preview.width(), preview.height()) - 1.0).abs();
        record["development"] = json!([width, height]);
        record["aspect_gap"] = json!(aspect_gap);
        if preview_landscape != development_landscape || aspect_gap > ASPECT_TOLERANCE {
            skip(
                &mut record,
                format!(
                    "preview {}x{} frames differently from the development {width}x{height}",
                    preview.width(),
                    preview.height()
                ),
            );
            records.push(record);
            continue;
        }
        let resized = image::imageops::resize(
            &preview,
            width,
            height,
            image::imageops::FilterType::Triangle,
        );
        resized
            .save(output.join(format!("{stem}.preview.png")))
            .unwrap();
        let mut file = File::create(output.join(format!("{stem}.dev"))).unwrap();
        file.write_all(b"LFDEV1").unwrap();
        file.write_all(&width.to_le_bytes()).unwrap();
        file.write_all(&height.to_le_bytes()).unwrap();
        let mut body = Vec::with_capacity(development.len() * 4);
        for value in &development {
            body.extend_from_slice(&value.to_le_bytes());
        }
        file.write_all(&body).unwrap();
        eprintln!(
            "{stem}: {} {} {width}x{height}",
            metadata.make, metadata.model
        );
        records.push(record);
    }
    fs::write(
        output.join("index.json"),
        serde_json::to_vec_pretty(&json!({ "files": records })).unwrap(),
    )
    .unwrap();
}
