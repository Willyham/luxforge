use crate::*;
use image::{ImageDecoder, ImageReader, Rgb, RgbImage};
pub const COLORS: [[u8; 3]; 4] = [[220, 35, 45], [35, 190, 65], [40, 70, 220], [235, 195, 30]];
pub const ORDERS: [[usize; 4]; 8] = [
    [0, 1, 2, 3],
    [1, 0, 3, 2],
    [3, 2, 1, 0],
    [2, 3, 0, 1],
    [0, 2, 1, 3],
    [2, 0, 3, 1],
    [3, 1, 2, 0],
    [1, 3, 0, 2],
];
fn pattern(w: u32, h: u32) -> RgbImage {
    let mut img = RgbImage::from_fn(w, h, |x, y| {
        Rgb(COLORS[(usize::from(y >= h / 2) * 2) + usize::from(x >= w / 2)])
    });
    for y in 30..h - 30 {
        for x in w / 2 - 1..=w / 2 + 1 {
            img.put_pixel(x, y, Rgb([255; 3]));
        }
    }
    for y in 30..55 {
        let radius = (y - 30) * 15 / 25;
        for x in w / 2 - radius..=w / 2 + radius {
            img.put_pixel(x, y, Rgb([255; 3]));
        }
    }
    for x in (20..150.min(w - 20)).step_by(4) {
        for y in h / 2 - 20..h / 2 + 20 {
            img.put_pixel(x, y, Rgb([0; 3]));
        }
    }
    img
}
fn encode(path: &Path, w: u32, h: u32) -> Result {
    let img = pattern(w, h);
    image::codecs::jpeg::JpegEncoder::new_with_quality(fs::File::create(path)?, 95)
        .encode_image(&img)?;
    Ok(())
}

/// The `mixer` smoke scenario's own fixture: a small, fully saturated hue wheel. The golden
/// orientation fixtures hold only four flat quadrant colours, with no continuous hue range to
/// inspect a mixer hue rotation's continuity across, so this generates one deterministically the
/// same way the quadrant pattern above does. Angle 0 (the wheel's own east point) is pure sRGB red,
/// where the colour mixer's own red range is centred, and angle continues counter-clockwise through
/// the spectrum; radius is saturation, full value throughout, so every ring but the centre is fully
/// saturated.
pub const HUE_WHEEL_SIZE: u32 = 480;

fn hsv_to_rgb(hue_deg: f32, saturation: f32, value: f32) -> [u8; 3] {
    let c = value * saturation;
    let h_prime = hue_deg / 60.0;
    let x = c * (1.0 - (h_prime.rem_euclid(2.0) - 1.0).abs());
    let (r1, g1, b1) = match h_prime as u32 % 6 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = value - c;
    [
        ((r1 + m) * 255.0).round().clamp(0.0, 255.0) as u8,
        ((g1 + m) * 255.0).round().clamp(0.0, 255.0) as u8,
        ((b1 + m) * 255.0).round().clamp(0.0, 255.0) as u8,
    ]
}

fn hue_wheel(size: u32) -> RgbImage {
    let radius = size as f32 / 2.0;
    // The near-grey canvas outside the circle, distinct enough from every saturated wheel colour
    // that a saturation threshold finds the wheel's own bounds without ever catching the backdrop.
    const BACKGROUND: Rgb<u8> = Rgb([40, 40, 42]);
    RgbImage::from_fn(size, size, |x, y| {
        let dx = x as f32 + 0.5 - radius;
        let dy = y as f32 + 0.5 - radius;
        let r = (dx * dx + dy * dy).sqrt();
        if r > radius {
            BACKGROUND
        } else {
            let hue = dy.atan2(dx).to_degrees().rem_euclid(360.0);
            let saturation = (r / radius).min(1.0);
            Rgb(hsv_to_rgb(hue, saturation, 1.0))
        }
    })
}

fn encode_wheel(path: &Path) -> Result {
    let img = hue_wheel(HUE_WHEEL_SIZE);
    image::codecs::jpeg::JpegEncoder::new_with_quality(fs::File::create(path)?, 95)
        .encode_image(&img)?;
    Ok(())
}

pub fn generate(out: &Path) -> Result {
    ensure(
        !out.exists(),
        "Generated fixture output must be new; choose --output or remove only disposable prior outputs",
    )?;
    fs::create_dir_all(out)?;
    let mut entries = Vec::new();
    for (w, h) in [(6000, 4000), (10000, 6000)] {
        let file = format!("{}mp.jpg", w * h / 1_000_000);
        let path = out.join(&file);
        encode(&path, w, h)?;
        entries.push(json!({"file":file,"width":w,"height":h,"sha256":hash(&path)?}));
    }
    let wheel_path = out.join("hue-wheel.jpg");
    encode_wheel(&wheel_path)?;
    entries.push(
        json!({"file":"hue-wheel.jpg","width":HUE_WHEEL_SIZE,"height":HUE_WHEEL_SIZE,"sha256":hash(&wheel_path)?}),
    );
    write_json(
        &out.join("manifest.json"),
        &json!({"generator":"Rust image 0.25.9 / xtask pattern-v1","entries":entries}),
    )?;
    println!(
        "Generated 24/60 MP and hue-wheel fixtures in {}",
        out.display()
    );
    Ok(())
}
pub fn check(root: &Path) -> Result {
    let dir = root.join("fixtures/s0");
    let m = read_json(&dir.join("manifest.json"))?;
    for e in m["entries"].as_array().ok_or("Fixture entries")? {
        let path = dir.join(e["file"].as_str().ok_or("Fixture filename")?);
        ensure(
            json!(hash(&path)?) == e["sha256"],
            format!("Fixture hash changed: {}", path.display()),
        )?;
        let result = lightwell_core::open(&path);
        if e["expected"] != "supported" {
            ensure(
                result.is_err(),
                format!("Unexpectedly accepted {}", path.display()),
            )?;
            ensure(
                result.err().unwrap().kind.code() == e["expected"].as_str().unwrap(),
                "Wrong fixture rejection",
            )?;
            continue;
        }
        let photo = result?;
        ensure(
            json!([photo.width, photo.height]) == json!([e["display_width"], e["display_height"]]),
            "Wrong oriented dimensions",
        )?;
        ensure(
            json!(photo.orientation) == e["orientation"],
            "Wrong orientation",
        )?;
        let mut decoder = ImageReader::open(&path)?
            .with_guessed_format()?
            .into_decoder()?;
        ensure(
            json!([decoder.dimensions().0, decoder.dimensions().1])
                == json!([e["width"], e["height"]]),
            "Wrong encoded dimensions",
        )?;
        ensure(
            (decoder.color_type() == image::ColorType::L8) == (e["mode"] == "L"),
            "Wrong grayscale mode",
        )?;
        if e["file"] == "srgb.jpg" {
            ensure(
                decoder.icc_profile()?.is_some_and(|p| p.len() > 128),
                "Missing sRGB profile",
            )?;
        }
        if e["file"].as_str().unwrap().starts_with("orientation-") {
            let order = ORDERS[(photo.orientation - 1) as usize];
            for ((x, y), i) in [
                (photo.width / 4, photo.height / 4),
                (3 * photo.width / 4, photo.height / 4),
                (photo.width / 4, 3 * photo.height / 4),
                (3 * photo.width / 4, 3 * photo.height / 4),
            ]
            .into_iter()
            .zip(order)
            {
                let offset = ((y * photo.width + x) * 4) as usize;
                ensure(
                    photo.rgba[offset..offset + 3]
                        .iter()
                        .zip(COLORS[i])
                        .all(|(a, b)| a.abs_diff(b) <= 5),
                    "Wrong fixture corner",
                )?;
            }
        }
    }
    let tmp = tempfile::tempdir()?;
    encode(&tmp.path().join("a.jpg"), 480, 320)?;
    encode(&tmp.path().join("b.jpg"), 480, 320)?;
    ensure(
        hash(&tmp.path().join("a.jpg"))? == hash(&tmp.path().join("b.jpg"))?,
        "Nondeterministic Rust generator",
    )?;
    println!(
        "PASS 16 preserved golden fixture hashes/content/errors and deterministic Rust generator"
    );
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn golden_corpus() {
        check(&root().unwrap()).unwrap();
    }
    #[test]
    fn generation_preserves_existing_directory() {
        let t = tempfile::tempdir().unwrap();
        fs::write(t.path().join("sentinel"), "keep").unwrap();
        assert!(generate(t.path()).is_err());
        assert_eq!(
            fs::read_to_string(t.path().join("sentinel")).unwrap(),
            "keep"
        );
    }
}
