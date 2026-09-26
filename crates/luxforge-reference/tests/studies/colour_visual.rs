//! Ignored visual review for the frozen Saturation/Vibrance reference.
//!
//! Not part of `cargo xtask check`. Run it explicitly, then inspect the
//! PNGs it writes to a temp directory (the test prints the directory path)
//! with an image viewer or the Read tool. Commit no images: this is a
//! one-time (and re-run-on-demand) design review, not a fixture.
//!
//! ```sh
//! cargo test --package luxforge-reference --test studies \
//!     -- --ignored --nocapture
//! ```

use image::{ImageBuffer, Rgb, RgbImage};
use luxforge_reference::colour::{self, Oklab};
use std::path::{Path, PathBuf};

fn code_to_linear(code: u8) -> f64 {
    let encoded = f64::from(code) / 255.0;
    if encoded <= 0.04045 {
        encoded / 12.92
    } else {
        ((encoded + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_code(linear: f64) -> u8 {
    let clamped = linear.clamp(0.0, 1.0);
    let encoded = if clamped <= 0.0031308 {
        clamped * 12.92
    } else {
        1.055 * clamped.powf(1.0 / 2.4) - 0.055
    };
    (255.0 * encoded + 0.5).floor().clamp(0.0, 255.0) as u8
}

/// The seven review points named in TASK-015: saturation at -100, -50, +50,
/// +100 (vibrance neutral) and vibrance at -100, +50, +100 (saturation
/// neutral).
fn review_points() -> Vec<(&'static str, f64, f64)> {
    vec![
        ("saturation_-100", 0.0, -100.0),
        ("saturation_-50", 0.0, -50.0),
        ("saturation_+50", 0.0, 50.0),
        ("saturation_+100", 0.0, 100.0),
        ("vibrance_-100", -100.0, 0.0),
        ("vibrance_+50", 50.0, 0.0),
        ("vibrance_+100", 100.0, 0.0),
    ]
}

fn apply_to_image(source: &RgbImage, vibrance: f64, saturation: f64) -> RgbImage {
    let (width, height) = source.dimensions();
    ImageBuffer::from_fn(width, height, |x, y| {
        let Rgb([r, g, b]) = *source.get_pixel(x, y);
        let linear = [code_to_linear(r), code_to_linear(g), code_to_linear(b)];
        let out = colour::apply_basic_colour(linear, vibrance, saturation);
        Rgb([
            linear_to_code(out[0]),
            linear_to_code(out[1]),
            linear_to_code(out[2]),
        ])
    })
}

/// A hue sweep (top third), a chroma sweep (middle third) and the three
/// documented skin-like patches (bottom third), all built directly in
/// Oklab and converted through the same reference the units under review
/// use, so the synthetic image starts from a valid in-gamut sRGB base.
fn synthetic_review_image(width: u32, height: u32) -> RgbImage {
    let band = height / 3;
    ImageBuffer::from_fn(width, height, |x, y| {
        let t = f64::from(x) / f64::from(width.max(1) - 1);
        let rgb = if y < band {
            // Hue sweep at fixed moderate chroma and lightness.
            let hue = t * 360.0;
            let hue_rad = hue.to_radians();
            let lab = Oklab {
                l: 0.70,
                a: 0.15 * hue_rad.cos(),
                b: 0.15 * hue_rad.sin(),
            };
            colour::from_oklab(lab)
        } else if y < 2 * band {
            // Chroma sweep at a fixed hue (30 degrees, orange-red) and lightness.
            let hue_rad: f64 = 30.0_f64.to_radians();
            let chroma = t * 0.30;
            let lab = Oklab {
                l: 0.70,
                a: chroma * hue_rad.cos(),
                b: chroma * hue_rad.sin(),
            };
            colour::from_oklab(lab)
        } else {
            // Three skin-like patches, side by side, from the task's list.
            let patches: [[u8; 3]; 3] = [[255, 219, 172], [224, 172, 140], [141, 85, 36]];
            let index = ((t * 3.0) as usize).min(2);
            let [r, g, b] = patches[index];
            [code_to_linear(r), code_to_linear(g), code_to_linear(b)]
        };
        Rgb([
            linear_to_code(rgb[0]),
            linear_to_code(rgb[1]),
            linear_to_code(rgb[2]),
        ])
    })
}

fn write_png(dir: &Path, name: &str, image: &RgbImage) {
    let path = dir.join(format!("{name}.png"));
    image
        .save(&path)
        .unwrap_or_else(|err| panic!("write {}: {err}", path.display()));
}

#[test]
#[ignore = "manual visual review; writes PNGs to a temp dir and prints the path"]
fn saturation_and_vibrance_visual_review() {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..");
    let source_path = repo_root
        .join("fixtures")
        .join("s0")
        .join("orientation-1.jpg");
    let source = image::open(&source_path)
        .unwrap_or_else(|err| panic!("open {}: {err}", source_path.display()))
        .to_rgb8();

    let out_dir = std::env::temp_dir().join("luxforge-basic-colour-visual-review");
    std::fs::create_dir_all(&out_dir).expect("create temp review directory");

    write_png(&out_dir, "orientation-1_original", &source);
    for (name, vibrance, saturation) in review_points() {
        let out = apply_to_image(&source, vibrance, saturation);
        write_png(&out_dir, &format!("orientation-1_{name}"), &out);
    }

    let synthetic = synthetic_review_image(480, 240);
    write_png(&out_dir, "synthetic_original", &synthetic);
    for (name, vibrance, saturation) in review_points() {
        let out = apply_to_image(&synthetic, vibrance, saturation);
        write_png(&out_dir, &format!("synthetic_{name}"), &out);
    }

    println!("wrote review PNGs to {}", out_dir.display());
}
