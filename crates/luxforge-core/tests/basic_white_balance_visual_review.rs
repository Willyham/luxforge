//! Ignored visual review for `docs/design/basic-white-balance.md`: renders the frozen reference
//! transform at six representative Temperature/Tint settings over a real photo and a synthetic
//! cast image, and separately checks the neutral picker on a known synthetic cast.
//!
//! Not run by `cargo test` (both tests are `#[ignore]`d) and commits no images: run explicitly
//! with `cargo test -p luxforge-core --test basic_white_balance_visual_review -- --ignored`,
//! look at the PNGs it writes under a temp directory, and record findings in the design doc.

mod reference;

use reference::white_balance::{apply, solve_from_patch};
use reference::{srgb_decode, srgb_encode, srgb_quantize};
use std::path::PathBuf;

const SETTINGS: [(f64, f64, &str); 6] = [
    (-100.0, 0.0, "temperature-minus100"),
    (-50.0, 0.0, "temperature-minus50"),
    (50.0, 0.0, "temperature-plus50"),
    (100.0, 0.0, "temperature-plus100"),
    (0.0, -100.0, "tint-minus100"),
    (0.0, 100.0, "tint-plus100"),
];

fn output_dir() -> PathBuf {
    let dir = std::env::temp_dir().join("luxforge-white-balance-review");
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn byte(v: f64) -> u8 {
    // Clamp only here, at the displayed-PNG output boundary; `apply` itself never clamps.
    srgb_quantize(srgb_encode(v.clamp(0.0, 1.0)))
}

fn apply_to_image(input: &image::RgbImage, temperature: f64, tint: f64) -> image::RgbImage {
    image::RgbImage::from_fn(input.width(), input.height(), |x, y| {
        let p = input.get_pixel(x, y);
        let linear = [srgb_decode(p[0]), srgb_decode(p[1]), srgb_decode(p[2])];
        let out = apply(temperature, tint, linear);
        image::Rgb([byte(out[0]), byte(out[1]), byte(out[2])])
    })
}

#[test]
#[ignore = "writes PNGs to a temp directory for manual visual review; not part of automated evidence"]
fn visual_review_orientation_1() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/s0/orientation-1.jpg"
    );
    let input = image::open(path).unwrap().to_rgb8();
    let dir = output_dir();
    input.save(dir.join("orientation-1-original.png")).unwrap();
    for (t, tint, name) in SETTINGS {
        let out = apply_to_image(&input, t, tint);
        out.save(dir.join(format!("orientation-1-{name}.png")))
            .unwrap();
    }
    println!("wrote orientation-1 review PNGs to {}", dir.display());
}

#[test]
#[ignore = "writes PNGs to a temp directory for manual visual review; not part of automated evidence"]
fn visual_review_synthetic_cast_and_picker() {
    // A synthetic two-tone grey checkerboard (a stand-in for a neutral grey card under a cast
    // light) with a known cast applied by multiplying its linear channels by a stated gain
    // triple: warm and slightly green, as a fluorescent-adjacent light source might cast.
    const CAST_GAIN: [f64; 3] = [1.25, 1.05, 0.65];
    let (width, height) = (64u32, 64u32);
    let mut cast = image::RgbImage::new(width, height);
    for y in 0..height {
        for x in 0..width {
            let base = if (x / 8 + y / 8) % 2 == 0 { 0.6 } else { 0.4 };
            let linear = [
                base * CAST_GAIN[0],
                base * CAST_GAIN[1],
                base * CAST_GAIN[2],
            ];
            cast.put_pixel(
                x,
                y,
                image::Rgb([byte(linear[0]), byte(linear[1]), byte(linear[2])]),
            );
        }
    }
    let dir = output_dir();
    cast.save(dir.join("synthetic-cast-original.png")).unwrap();

    for (t, tint, name) in SETTINGS {
        let out = apply_to_image(&cast, t, tint);
        out.save(dir.join(format!("synthetic-cast-{name}.png")))
            .unwrap();
    }

    // Neutral picker: sample a 5x5 patch from one grey tile (a uniform-cast region) and check the
    // solver recovers a correction that visibly neutralizes it.
    let patch: Vec<[u8; 3]> = (0..25)
        .map(|i| {
            let px = 4 + i % 5;
            let py = 4 + i / 5;
            let p = cast.get_pixel(px, py);
            [p[0], p[1], p[2]]
        })
        .collect();
    let solved = solve_from_patch(&patch);
    println!("picker on cast patch -> {solved:?}");
    if let Ok((t, tint)) = solved {
        let corrected = apply_to_image(&cast, t as f64, tint as f64);
        corrected
            .save(dir.join("synthetic-cast-corrected-by-picker.png"))
            .unwrap();
        let corrected_patch = corrected.get_pixel(6, 6);
        println!(
            "corrected patch centre pixel (should be near-neutral): {:?}",
            corrected_patch.0
        );
    }
    println!("wrote synthetic cast review PNGs to {}", dir.display());
}
