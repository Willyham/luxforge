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

/// The `presence` smoke scenario's own fixture: a quadrant pattern of plain greys, deterministic
/// like the ones above, holding what Texture, Clarity and Dehaze each need to show something —
/// none of which the golden `orientation-1` fixture's hard edges and flat colour fields have on
/// their own:
///
/// - top-left: a smooth horizontal gradient, floor to ceiling (no flat run for a filter to no-op
///   on, and nothing sharp for one to catch on);
/// - top-right: a hard vertical step, the only sharp edge in the image, where Clarity's local
///   contrast gain shows as a widened step and, at 100%, a visible halo either side of it;
/// - bottom-left: a low-amplitude checker (8 px period, a 20-code light/dark gap) at Texture's own
///   medium-frequency band, which Texture's gain widens the light/dark gap of. The gap is kept
///   small deliberately: the frozen guided-filter pair (`docs/design/presence-study.md`) drives its
///   blend weight toward 1 on a strong edge so both smoothers reproduce it and the band cancels,
///   which is what bounds Texture's own overshoot; a high-contrast checker would read as an edge
///   and Texture would leave it alone, exactly like the one it is asked to leave unaffected;
/// - bottom-right: a flat mid-grey, the same "does an unaffected region stay unaffected" control
///   the other two scenarios' corner/centre patches are, deep enough in from every edge to clear
///   Clarity's own reduced-grid base radius.
///
/// Every level is kept at 40 or above so the darkest deliberate content never approaches the dark
/// canvas background a bounds scan excludes chrome by.
pub const PRESENCE_FIXTURE: (u32, u32) = (1440, 960);
const PRESENCE_FLOOR: f32 = 40.0;
const PRESENCE_CEILING: f32 = 255.0;
/// The step edge's own two levels: a narrower 140-code gap than the gradient's own floor-to-ceiling
/// run, so Clarity's local-contrast gain has headroom to widen the step on both sides without
/// immediately clamping against 0 or 255 (a step already touching 255 leaves the gain nothing to
/// brighten further into, which is what a first attempt at this fixture, spanning the full 40..255
/// range, measured: the low side undershot visibly but the high side, already at the output
/// ceiling, could not move at all).
const PRESENCE_EDGE_LOW: u8 = 70;
const PRESENCE_EDGE_HIGH: u8 = 210;
/// The checker's own two levels: a 20-code gap, well under the guided filter's own regularisation
/// (`EPS_TEXTURE`, about a 13-code window deviation), so the pair still treats it as textured
/// content to blend rather than an edge to reproduce.
const PRESENCE_CHECKER_LOW: u8 = 118;
const PRESENCE_CHECKER_HIGH: u8 = 138;
/// The checker cell width in source pixels (an 8 px light/dark period): measured against the
/// frozen units directly (`cargo test --package luxforge-reference --test studies`, a
/// temporary local probe of `band_gain` at this fixture's 1440 px long side), the widest gain the
/// fine (1 px) and coarse (2 px) guided radii produce together falls at a 6–8 px period; a 2 px
/// period is not a usable probe at all (`texture_is_a_medium_frequency_band` in
/// `studies/presence.rs` notes a period-2 sinusoid samples to a constant on an integer grid).
const PRESENCE_CHECKER_PERIOD: u32 = 4;

fn presence_fixture(w: u32, h: u32) -> RgbImage {
    let (hw, hh) = (w / 2, h / 2);
    RgbImage::from_fn(w, h, |x, y| {
        let level = if y < hh {
            if x < hw {
                // Top-left: smooth horizontal gradient.
                let t = x as f32 / hw as f32;
                (PRESENCE_FLOOR + t * (PRESENCE_CEILING - PRESENCE_FLOOR)).round() as u8
            } else {
                // Top-right: a hard vertical step at the quadrant's own midpoint.
                let local_x = x - hw;
                if local_x < hw / 2 {
                    PRESENCE_EDGE_LOW
                } else {
                    PRESENCE_EDGE_HIGH
                }
            }
        } else if x < hw {
            // Bottom-left: a low-amplitude checker.
            let local_x = x;
            let local_y = y - hh;
            let checker = (local_x / PRESENCE_CHECKER_PERIOD + local_y / PRESENCE_CHECKER_PERIOD)
                .is_multiple_of(2);
            if checker {
                PRESENCE_CHECKER_LOW
            } else {
                PRESENCE_CHECKER_HIGH
            }
        } else {
            // Bottom-right: a flat mid-grey.
            128
        };
        Rgb([level; 3])
    })
}

fn encode_presence(path: &Path) -> Result {
    let (w, h) = PRESENCE_FIXTURE;
    let img = presence_fixture(w, h);
    image::codecs::jpeg::JpegEncoder::new_with_quality(fs::File::create(path)?, 95)
        .encode_image(&img)?;
    Ok(())
}

/// The `mask-range` smoke scenario's own fixture: twelve flat patches of the 24-patch reflective
/// colour chart's own measured sRGB renderings, which is what the
/// [range study](../../docs/design/range-study.md) measured every one of its figures over. Nothing
/// here is a colour chosen to make a point: the patches are that chart's, and the failures they
/// produce are the study's own, reproduced in a photograph the editor renders.
///
/// The layout is what makes each of the study's named limits legible in one frame:
///
/// - `blue_sky` and `neutral5` sit side by side in the top row and are **1.43 output codes apart**
///   on the luminance axis (`47.254` and `47.815`), so no band separates them and a band drawn for
///   the sky takes the grey card with it;
/// - `light_skin` and `dark_skin` are `0.0108` apart in the frozen colour metric, a third of what one
///   face's own shading spans, so one colour range takes both;
/// - `white`, `neutral8`, `neutral65`, `neutral5` and `black` are mutually within `0.0015`, so a
///   sampled grey selects the whole tonal range;
/// - `blue_sky` appears **twice**, in the top row and the bottom row, so a gradient that reaches one
///   and not the other gives every reading a control of the identical colour inside the same frame.
///   That is what lets the scenario prove a range selection follows the operation's *input* without
///   predicting an output code;
/// - `foliage` sits beside the bottom `blue_sky`, so one brush stroke crosses two surfaces and a
///   stroke held to the sky's colour can be read against the same stroke unheld;
/// - `orange` and `red` are the cases that work: far enough away in colour to stay out of a
///   selection the near ones fall into.
///
/// Every patch is flat and its centre is 180 px from the nearest boundary, so a JPEG's chroma
/// subsampling and its ringing stay at the edges and a probe reads the colour the chart specifies.
pub const RANGE_FIXTURE: (u32, u32) = (1440, 960);
pub const RANGE_COLUMNS: u32 = 4;
pub const RANGE_ROWS: u32 = 3;

/// The twelve patches in row-major order, each an sRGB rendering of one chart patch. The names are
/// the chart's own and are what the scenario's probes are called.
pub const RANGE_PATCHES: [(&str, [u8; 3]); 12] = [
    ("sky-top", [98, 122, 157]),
    ("grey-card", [122, 122, 121]),
    ("light-skin", [194, 150, 130]),
    ("dark-skin", [115, 82, 68]),
    ("white", [243, 243, 242]),
    ("grey-65", [160, 160, 160]),
    ("black", [52, 52, 52]),
    ("orange", [214, 126, 44]),
    ("sky-bottom", [98, 122, 157]),
    ("foliage", [87, 108, 67]),
    ("grey-8", [200, 200, 200]),
    ("red", [175, 54, 60]),
];

fn range_fixture(w: u32, h: u32) -> RgbImage {
    let (cell_w, cell_h) = (w / RANGE_COLUMNS, h / RANGE_ROWS);
    RgbImage::from_fn(w, h, |x, y| {
        let column = (x / cell_w).min(RANGE_COLUMNS - 1);
        let row = (y / cell_h).min(RANGE_ROWS - 1);
        Rgb(RANGE_PATCHES[(row * RANGE_COLUMNS + column) as usize].1)
    })
}

fn encode_range(path: &Path) -> Result {
    let (w, h) = RANGE_FIXTURE;
    let img = range_fixture(w, h);
    image::codecs::jpeg::JpegEncoder::new_with_quality(fs::File::create(path)?, 95)
        .encode_image(&img)?;
    Ok(())
}

/// One JPEG the rendered and timing tiers need before they can run: its file name inside a
/// fixtures directory, the function that writes it, and the manifest fields it needs beyond `file`
/// and `sha256` (both of which `generate` fills in from what it actually wrote, once it has hashed
/// the bytes). `generate` and `verify` both read this one table, so a fixture can never be listed
/// for one and forgotten by the other.
pub struct Fixture {
    pub file: &'static str,
    write: fn(&Path) -> Result,
    manifest: fn() -> Value,
}

pub const TABLE: [Fixture; 5] = [
    Fixture {
        file: "24mp.jpg",
        write: |p| encode(p, 6000, 4000),
        manifest: || json!({"width":6000,"height":4000}),
    },
    Fixture {
        file: "60mp.jpg",
        write: |p| encode(p, 10000, 6000),
        manifest: || json!({"width":10000,"height":6000}),
    },
    Fixture {
        file: "hue-wheel.jpg",
        write: encode_wheel,
        manifest: || json!({"width":HUE_WHEEL_SIZE,"height":HUE_WHEEL_SIZE}),
    },
    Fixture {
        file: "presence.jpg",
        write: encode_presence,
        manifest: || {
            let (w, h) = PRESENCE_FIXTURE;
            json!({"width":w,"height":h})
        },
    },
    Fixture {
        file: "range.jpg",
        write: encode_range,
        manifest: || {
            let (w, h) = RANGE_FIXTURE;
            json!({
                "width":w,"height":h,
                "patches":RANGE_PATCHES.map(|(name,codes)| json!({"name":name,"srgb":codes})),
            })
        },
    },
];

/// Whether every fixture in `table` already exists inside `dir`. `verify` reads this over
/// [`TABLE`] to decide whether a tier needs to schedule `generate-fixtures` at all, rather than
/// keeping its own second list of the same file names.
pub fn present(dir: &Path) -> bool {
    TABLE.iter().all(|f| dir.join(f.file).is_file())
}

/// Write every fixture in `table` into `out` that is not already there with a hash matching its
/// recorded manifest entry. An existing, correctly-hashed file is left untouched; a file whose
/// bytes no longer match its own recorded hash, or that exists with no recorded entry at all, is
/// refused rather than silently overwritten, so a caller never loses evidence of what changed it.
/// Safe to call repeatedly against the same directory: a second call with nothing missing writes
/// nothing new and leaves every file exactly as it was.
fn generate_table(table: &[Fixture], out: &Path) -> Result {
    fs::create_dir_all(out)?;
    let manifest_path = out.join("manifest.json");
    let mut entries: Vec<Value> = if manifest_path.is_file() {
        read_json(&manifest_path)?["entries"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    for fixture in table {
        let path = out.join(fixture.file);
        let recorded = entries.iter().position(|e| e["file"] == fixture.file);
        if let Some(index) = recorded {
            if path.is_file() {
                let actual = hash(&path)?;
                ensure(
                    json!(actual) == entries[index]["sha256"],
                    format!(
                        "{} does not match its recorded manifest hash; remove it and its manifest entry to regenerate it rather than overwrite it in place",
                        fixture.file
                    ),
                )?;
                continue;
            }
        } else {
            ensure(
                !path.exists(),
                format!(
                    "{} exists with no matching manifest entry; remove the stray file or the manifest before regenerating",
                    fixture.file
                ),
            )?;
        }
        (fixture.write)(&path)?;
        let mut entry = (fixture.manifest)();
        entry["file"] = json!(fixture.file);
        entry["sha256"] = json!(hash(&path)?);
        match recorded {
            Some(index) => entries[index] = entry,
            None => entries.push(entry),
        }
    }
    write_json(
        &manifest_path,
        &json!({"generator":"Rust image 0.25.9 / xtask pattern-v1","entries":entries}),
    )?;
    Ok(())
}

pub fn generate(out: &Path) -> Result {
    generate_table(&TABLE, out)?;
    println!(
        "Fixtures ready ({}) in {}",
        TABLE.iter().map(|f| f.file).collect::<Vec<_>>().join(", "),
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
        let result = luxforge_core::open_source(&path);
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
        let source = result?;
        ensure(
            json!([source.width, source.height])
                == json!([e["display_width"], e["display_height"]]),
            "Wrong oriented dimensions",
        )?;
        ensure(
            json!(source.orientation) == e["orientation"],
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
            let order = ORDERS[(source.orientation - 1) as usize];
            for ((x, y), i) in [
                (source.width / 4, source.height / 4),
                (3 * source.width / 4, source.height / 4),
                (source.width / 4, 3 * source.height / 4),
                (3 * source.width / 4, 3 * source.height / 4),
            ]
            .into_iter()
            .zip(order)
            {
                let offset = ((y * source.width + x) * 4) as usize;
                ensure(
                    source.rgba[offset..offset + 3]
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

    /// A stand-in for [`TABLE`] built from the same generator the real 24/60 MP entries use, at
    /// sizes small enough for the mechanism tests below to run in milliseconds. `generate`'s own
    /// logic lives in `generate_table`, which both this table and the real one go through, so
    /// exercising it here proves the same idempotency and refusal behaviour `generate` gives the
    /// real fixtures without ever encoding a 24 or 60 MP image in a test.
    fn tiny_table() -> [Fixture; 2] {
        [
            Fixture {
                file: "a.jpg",
                // `pattern` draws its centre mark down to y = 54, so this stays the smallest size
                // that clears it rather than a size chosen only for speed.
                write: |p| encode(p, 200, 120),
                manifest: || json!({"width":200,"height":120}),
            },
            Fixture {
                file: "b.jpg",
                write: |p| encode(p, 160, 100),
                manifest: || json!({"width":160,"height":100}),
            },
        ]
    }

    #[test]
    fn generation_is_safe_to_run_twice_and_produces_byte_identical_files() {
        let t = tempfile::tempdir().unwrap();
        let table = tiny_table();
        generate_table(&table, t.path()).unwrap();
        let before: Vec<Vec<u8>> = table
            .iter()
            .map(|f| fs::read(t.path().join(f.file)).unwrap())
            .collect();
        let manifest_before = fs::read(t.path().join("manifest.json")).unwrap();
        generate_table(&table, t.path()).unwrap();
        for (f, expected) in table.iter().zip(&before) {
            assert_eq!(
                &fs::read(t.path().join(f.file)).unwrap(),
                expected,
                "{} rewritten on a second, unchanged run",
                f.file
            );
        }
        assert_eq!(
            fs::read(t.path().join("manifest.json")).unwrap(),
            manifest_before,
            "manifest rewritten on a second, unchanged run"
        );
    }

    #[test]
    fn generation_fills_only_what_is_missing() {
        let t = tempfile::tempdir().unwrap();
        let table = tiny_table();
        generate_table(&table, t.path()).unwrap();
        let kept = fs::read(t.path().join("a.jpg")).unwrap();
        fs::remove_file(t.path().join("b.jpg")).unwrap();
        assert!(!present_in(&table, t.path()));
        generate_table(&table, t.path()).unwrap();
        assert!(t.path().join("b.jpg").is_file());
        assert_eq!(
            fs::read(t.path().join("a.jpg")).unwrap(),
            kept,
            "untouched fixture rewritten while filling in a missing one"
        );
        let manifest: Value = read_json(&t.path().join("manifest.json")).unwrap();
        let entry = manifest["entries"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["file"] == "b.jpg")
            .unwrap();
        assert_eq!(
            entry["sha256"],
            json!(hash(&t.path().join("b.jpg")).unwrap())
        );
    }

    fn present_in(table: &[Fixture], dir: &Path) -> bool {
        table.iter().all(|f| dir.join(f.file).is_file())
    }

    #[test]
    fn generation_refuses_a_hash_mismatch_rather_than_overwriting_it() {
        let t = tempfile::tempdir().unwrap();
        let table = tiny_table();
        generate_table(&table, t.path()).unwrap();
        fs::write(t.path().join("a.jpg"), b"corrupted").unwrap();
        let error = generate_table(&table, t.path()).unwrap_err().to_string();
        assert!(error.contains("a.jpg"), "{error}");
        assert_eq!(
            fs::read(t.path().join("a.jpg")).unwrap(),
            b"corrupted",
            "a hash mismatch must not be silently overwritten"
        );
    }

    #[test]
    fn a_directory_missing_only_range_jpg_is_not_present() {
        let t = tempfile::tempdir().unwrap();
        for f in TABLE.iter().filter(|f| f.file != "range.jpg") {
            fs::write(t.path().join(f.file), b"stub").unwrap();
        }
        assert!(!present(t.path()), "missing range.jpg still reads present");
        fs::write(t.path().join("range.jpg"), b"stub").unwrap();
        assert!(present(t.path()));
    }
}
