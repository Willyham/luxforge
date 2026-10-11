//! `sheet`: per-case contact sheets of `generate`'s output, for inspection.

use std::path::{Path, PathBuf};

use crate::args::Args;
use crate::image::Rgb8;

pub const USAGE: &str = "  sheet --dir DIR [--out DIR] [--tile N] [--columns LABEL[,LABEL...]]
                                           one contact sheet per case directory in DIR (generate's
                                           --out): input, truth, then every candidate image
                                           (LABEL-sN.png; --columns picks and orders labels), each
                                           tile N px (default 384) under its label; written to
                                           DIR/sheets/CASE.png unless --out is given";

pub fn run(mut a: Args) -> Result<(), String> {
    let dir = a.path("--dir")?.ok_or("--dir is required")?;
    let out = a.path("--out")?.unwrap_or_else(|| dir.join("sheets"));
    let tile: u32 = a.parsed("--tile")?.unwrap_or(384);
    let columns: Option<Vec<String>> = a
        .opt("--columns")?
        .map(|c| c.split(',').map(|s| s.trim().to_string()).collect());
    a.done()?;
    if !(64..=2048).contains(&tile) {
        return Err("--tile must be between 64 and 2048".into());
    }
    std::fs::create_dir_all(&out).map_err(|e| format!("{}: {e}", out.display()))?;
    let mut cases: Vec<PathBuf> = std::fs::read_dir(&dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.join("input.png").exists())
        .collect();
    cases.sort();
    if cases.is_empty() {
        return Err(format!("{} has no case directories", dir.display()));
    }
    for case in &cases {
        let tiles = case_tiles(case, columns.as_deref())?;
        let sheet = compose(&tiles, tile)?;
        let name = case.file_name().unwrap().to_string_lossy().into_owned();
        let path = out.join(format!("{name}.png"));
        sheet.save_png(&path)?;
        println!(
            "{}: {}",
            path.display(),
            tiles
                .iter()
                .map(|(l, _)| l.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    Ok(())
}

/// A case's tiles in order: input, truth, then the candidate images, as (label, path).
pub fn case_tiles(
    case: &Path,
    columns: Option<&[String]>,
) -> Result<Vec<(String, PathBuf)>, String> {
    let mut tiles = vec![("input".to_string(), case.join("input.png"))];
    if case.join("truth.png").exists() {
        tiles.push(("truth".to_string(), case.join("truth.png")));
    }
    let mut names: Vec<String> = std::fs::read_dir(case)
        .map_err(|e| format!("{}: {e}", case.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| {
            n.ends_with(".png")
                && !n.ends_with("-model.png")
                && n != "input.png"
                && n != "truth.png"
        })
        .collect();
    names.sort();
    match columns {
        None => tiles.extend(
            names
                .into_iter()
                .map(|n| (n.trim_end_matches(".png").to_string(), case.join(&n))),
        ),
        Some(cols) => {
            for col in cols {
                let prefix = format!("{col}-s");
                tiles.extend(
                    names
                        .iter()
                        .filter(|n| {
                            n.starts_with(&prefix)
                                && n[prefix.len()..]
                                    .trim_end_matches(".png")
                                    .parse::<u64>()
                                    .is_ok()
                        })
                        .map(|n| (n.trim_end_matches(".png").to_string(), case.join(n))),
                );
            }
        }
    }
    Ok(tiles)
}

/// The label strip's height: 7-row glyphs at twice their size, with a 3 px margin.
const SCALE: u32 = 2;
const STRIP: u32 = 7 * SCALE + 6;

/// Lays the tiles out in one row, each resized to `tile`² under a black label strip.
pub fn compose(tiles: &[(String, PathBuf)], tile: u32) -> Result<Rgb8, String> {
    let width = tile * tiles.len() as u32;
    let height = tile + STRIP;
    let mut sheet = Rgb8::new(width, height, vec![0; width as usize * height as usize * 3]);
    for (i, (label, path)) in tiles.iter().enumerate() {
        let image = Rgb8::load(path)?.resized(tile, tile);
        let x0 = i as u32 * tile;
        for y in 0..tile {
            let src = (y * tile) as usize * 3;
            let dst = (((y + STRIP) * width + x0) as usize) * 3;
            sheet.data[dst..dst + tile as usize * 3]
                .copy_from_slice(&image.data[src..src + tile as usize * 3]);
        }
        draw_text(&mut sheet, x0 + 4, 3, label, tile - 8);
    }
    Ok(sheet)
}

/// Draws `text` in white, upper-cased, clipped to `max_width` pixels.
pub fn draw_text(image: &mut Rgb8, x: u32, y: u32, text: &str, max_width: u32) {
    let advance = 6 * SCALE;
    for (k, ch) in text.chars().enumerate() {
        let gx = x + k as u32 * advance;
        if (k as u32 + 1) * advance > max_width {
            break;
        }
        for (row, bits) in glyph(ch).iter().enumerate() {
            for col in 0..5u32 {
                if bits & (0x10 >> col) == 0 {
                    continue;
                }
                for dy in 0..SCALE {
                    for dx in 0..SCALE {
                        let (px, py) = (gx + col * SCALE + dx, y + row as u32 * SCALE + dy);
                        if px < image.width && py < image.height {
                            let i = (py as usize * image.width as usize + px as usize) * 3;
                            image.data[i..i + 3].copy_from_slice(&[255, 255, 255]);
                        }
                    }
                }
            }
        }
    }
}

/// A 5×7 glyph, one byte per row with the leftmost column in bit 4. Letters render upper-case;
/// characters without a glyph render as a box.
pub fn glyph(ch: char) -> [u8; 7] {
    match ch.to_ascii_uppercase() {
        '0' => [0x0E, 0x11, 0x13, 0x15, 0x19, 0x11, 0x0E],
        '1' => [0x04, 0x0C, 0x04, 0x04, 0x04, 0x04, 0x0E],
        '2' => [0x0E, 0x11, 0x01, 0x02, 0x04, 0x08, 0x1F],
        '3' => [0x1F, 0x02, 0x04, 0x02, 0x01, 0x11, 0x0E],
        '4' => [0x02, 0x06, 0x0A, 0x12, 0x1F, 0x02, 0x02],
        '5' => [0x1F, 0x10, 0x1E, 0x01, 0x01, 0x11, 0x0E],
        '6' => [0x06, 0x08, 0x10, 0x1E, 0x11, 0x11, 0x0E],
        '7' => [0x1F, 0x01, 0x02, 0x04, 0x08, 0x08, 0x08],
        '8' => [0x0E, 0x11, 0x11, 0x0E, 0x11, 0x11, 0x0E],
        '9' => [0x0E, 0x11, 0x11, 0x0F, 0x01, 0x02, 0x0C],
        'A' => [0x0E, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11],
        'B' => [0x1E, 0x11, 0x11, 0x1E, 0x11, 0x11, 0x1E],
        'C' => [0x0E, 0x11, 0x10, 0x10, 0x10, 0x11, 0x0E],
        'D' => [0x1C, 0x12, 0x11, 0x11, 0x11, 0x12, 0x1C],
        'E' => [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x1F],
        'F' => [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x10],
        'G' => [0x0E, 0x11, 0x10, 0x17, 0x11, 0x11, 0x0F],
        'H' => [0x11, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11],
        'I' => [0x0E, 0x04, 0x04, 0x04, 0x04, 0x04, 0x0E],
        'J' => [0x07, 0x02, 0x02, 0x02, 0x02, 0x12, 0x0C],
        'K' => [0x11, 0x12, 0x14, 0x18, 0x14, 0x12, 0x11],
        'L' => [0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x1F],
        'M' => [0x11, 0x1B, 0x15, 0x15, 0x11, 0x11, 0x11],
        'N' => [0x11, 0x11, 0x19, 0x15, 0x13, 0x11, 0x11],
        'O' => [0x0E, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E],
        'P' => [0x1E, 0x11, 0x11, 0x1E, 0x10, 0x10, 0x10],
        'Q' => [0x0E, 0x11, 0x11, 0x11, 0x15, 0x12, 0x0D],
        'R' => [0x1E, 0x11, 0x11, 0x1E, 0x14, 0x12, 0x11],
        'S' => [0x0F, 0x10, 0x10, 0x0E, 0x01, 0x01, 0x1E],
        'T' => [0x1F, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04],
        'U' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E],
        'V' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x0A, 0x04],
        'W' => [0x11, 0x11, 0x11, 0x15, 0x15, 0x15, 0x0A],
        'X' => [0x11, 0x11, 0x0A, 0x04, 0x0A, 0x11, 0x11],
        'Y' => [0x11, 0x11, 0x11, 0x0A, 0x04, 0x04, 0x04],
        'Z' => [0x1F, 0x01, 0x02, 0x04, 0x08, 0x10, 0x1F],
        '-' => [0x00, 0x00, 0x00, 0x1F, 0x00, 0x00, 0x00],
        '.' => [0x00, 0x00, 0x00, 0x00, 0x00, 0x0C, 0x0C],
        '_' => [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x1F],
        ' ' => [0x00; 7],
        _ => [0x1F, 0x11, 0x11, 0x11, 0x11, 0x11, 0x1F],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glyphs_fit_five_columns_and_letters_ignore_case() {
        for c in "0123456789abcdefghijklmnopqrstuvwxyz-._ ?".chars() {
            assert!(glyph(c).iter().all(|&row| row < 0x20), "{c}");
        }
        assert_eq!(glyph('k'), glyph('K'));
        assert_ne!(glyph('0'), glyph('O'));
    }

    #[test]
    fn a_sheet_has_one_labelled_tile_per_image_in_order() {
        let dir = std::env::temp_dir().join(format!("sheet-test-{}", std::process::id()));
        let case = dir.join("case-a");
        std::fs::create_dir_all(&case).unwrap();
        let solid = |v: u8| Rgb8::new(8, 8, vec![v; 8 * 8 * 3]);
        solid(10).save_png(&case.join("input.png")).unwrap();
        solid(20).save_png(&case.join("truth.png")).unwrap();
        solid(30).save_png(&case.join("lama-512-s0.png")).unwrap();
        solid(40)
            .save_png(&case.join("lama-512-s0-model.png"))
            .unwrap();
        solid(50)
            .save_png(&case.join("klein-q4-512-s0.png"))
            .unwrap();
        solid(60)
            .save_png(&case.join("klein-q4-512-s1.png"))
            .unwrap();

        let all = case_tiles(&case, None).unwrap();
        let labels: Vec<_> = all.iter().map(|(l, _)| l.as_str()).collect();
        assert_eq!(
            labels,
            [
                "input",
                "truth",
                "klein-q4-512-s0",
                "klein-q4-512-s1",
                "lama-512-s0"
            ]
        );

        let picked = case_tiles(
            &case,
            Some(&["lama-512".to_string(), "klein-q4-512".to_string()]),
        )
        .unwrap();
        let labels: Vec<_> = picked.iter().map(|(l, _)| l.as_str()).collect();
        assert_eq!(
            labels,
            [
                "input",
                "truth",
                "lama-512-s0",
                "klein-q4-512-s0",
                "klein-q4-512-s1"
            ]
        );

        let sheet = compose(&picked, 64).unwrap();
        assert_eq!((sheet.width, sheet.height), (64 * 5, 64 + STRIP));
        // Each tile's body is its source colour; its strip carries some white label pixels.
        for (i, v) in [10u8, 20, 30, 50, 60].iter().enumerate() {
            assert_eq!(sheet.pixel(i as u32 * 64 + 32, STRIP + 32), [*v; 3]);
            let strip_white =
                (0..STRIP).any(|y| (0..64).any(|x| sheet.pixel(i as u32 * 64 + x, y) == [255; 3]));
            assert!(strip_white, "tile {i} has no label");
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
