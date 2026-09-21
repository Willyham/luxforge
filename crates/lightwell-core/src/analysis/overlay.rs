//! The bounded display overlay reduction for the clipping indicators.
//!
//! The histogram and clipping contract forbids a full-resolution mask: the overlay is derived from
//! the same final raster the histogram was reduced from and only the **display-sized** buffer is
//! produced. Each display cell ORs [`clip_class`](super::clip_class) over every source pixel that
//! maps to it, so an isolated clipped pixel survives a Fit reduction instead of disappearing into a
//! blurred thumbnail, and a cell grid equal to the image is one to one.

use super::{Clip, PARALLEL_REDUCE_PIXELS, clip_class};
use crate::{Error, ErrorKind};
use rayon::prelude::*;

/// No channel of any source pixel in this cell sits at an output endpoint.
pub const OVERLAY_NONE: u8 = 0;
/// At least one source pixel in this cell has a channel at code 0.
pub const OVERLAY_SHADOW: u8 = 1;
/// At least one source pixel in this cell has a channel at code 255.
pub const OVERLAY_HIGHLIGHT: u8 = 2;
/// The cell holds both, whether in one pixel or in different ones.
pub const OVERLAY_BOTH: u8 = OVERLAY_SHADOW | OVERLAY_HIGHLIGHT;

/// The largest overlay grid, per side. The overlay is bounded by the viewport, and 4096 cells a
/// side is far beyond any display while keeping the buffer at 16 MiB in the worst case.
pub const MAX_OVERLAY_CELLS: u32 = 4096;

/// The bit(s) one clipping class contributes to its cell.
const fn bits(clip: Option<Clip>) -> u8 {
    match clip {
        None => OVERLAY_NONE,
        Some(Clip::Shadow) => OVERLAY_SHADOW,
        Some(Clip::Highlight) => OVERLAY_HIGHLIGHT,
        Some(Clip::Both) => OVERLAY_BOTH,
    }
}

/// Reduce one rendered raster to a display-sized clipping overlay: one byte per cell, `0` none,
/// `1` shadow, `2` highlight, `3` both. A source pixel `(x, y)` contributes to the cell
/// `(floor(x * cells_w / width), floor(y * cells_h / height))`, which is the same nearest-cell rule
/// the canvas uses, so nothing is averaged away and one clipped pixel still lights its cell.
///
/// Serial below one megapixel and on the shared Rayon pool above it, with worker-local cell buffers
/// merged by OR. Nothing proportional to the image is allocated: the only buffers are the cell
/// grids, bounded by [`MAX_OVERLAY_CELLS`] a side.
pub fn overlay(
    rgba: &[u8],
    width: u32,
    height: u32,
    cells_w: u32,
    cells_h: u32,
) -> Result<Vec<u8>, Error> {
    if width == 0 || height == 0 || cells_w == 0 || cells_h == 0 {
        return Err(Error::new(
            ErrorKind::Validation,
            "an overlay needs a non-empty image and a non-empty cell grid",
        ));
    }
    if cells_w > MAX_OVERLAY_CELLS || cells_h > MAX_OVERLAY_CELLS {
        return Err(Error::new(
            ErrorKind::ResourceLimit,
            format!(
                "an overlay of {cells_w}x{cells_h} cells exceeds the {MAX_OVERLAY_CELLS} cells a side the display overlay allows"
            ),
        ));
    }
    let pixels = u64::from(width) * u64::from(height);
    let expected = usize::try_from(pixels * 4).map_err(|_| {
        Error::new(
            ErrorKind::ResourceLimit,
            "image allocation is not addressable",
        )
    })?;
    if rgba.len() != expected {
        return Err(Error::new(
            ErrorKind::Validation,
            format!(
                "pixel buffer holds {} bytes, expected {expected} for a {width}x{height} image",
                rgba.len()
            ),
        ));
    }
    let cells = (cells_w as usize) * (cells_h as usize);
    let row_bytes = (width as usize) * 4;
    if pixels >= PARALLEL_REDUCE_PIXELS {
        // Worker-local cell grids, merged by OR: the reduction is associative, so the merge order
        // does not change the result.
        let merged = rgba
            .par_chunks_exact(row_bytes)
            .enumerate()
            .fold(
                || vec![OVERLAY_NONE; cells],
                |mut local, (y, row)| {
                    fold_row(&mut local, row, y as u32, width, height, cells_w, cells_h);
                    local
                },
            )
            .reduce(
                || vec![OVERLAY_NONE; cells],
                |mut a, b| {
                    for (left, right) in a.iter_mut().zip(b) {
                        *left |= right;
                    }
                    a
                },
            );
        return Ok(merged);
    }
    let mut grid = vec![OVERLAY_NONE; cells];
    for (y, row) in rgba.chunks_exact(row_bytes).enumerate() {
        fold_row(&mut grid, row, y as u32, width, height, cells_w, cells_h);
    }
    Ok(grid)
}

/// OR one source row into a cell grid. The row's cell row is computed once; the column index
/// advances per pixel.
fn fold_row(
    grid: &mut [u8],
    row: &[u8],
    y: u32,
    width: u32,
    height: u32,
    cells_w: u32,
    cells_h: u32,
) {
    let cell_y = cell_index(y, height, cells_h) as usize;
    let base = cell_y * cells_w as usize;
    for (x, pixel) in row.chunks_exact(4).enumerate() {
        let class = bits(clip_class([pixel[0], pixel[1], pixel[2], pixel[3]]));
        if class == OVERLAY_NONE {
            continue;
        }
        let cell_x = cell_index(x as u32, width, cells_w) as usize;
        grid[base + cell_x] |= class;
    }
}

/// `floor(index * cells / extent)`, computed in 64 bits and clamped to the last cell so a rounding
/// edge can never index past the grid.
fn cell_index(index: u32, extent: u32, cells: u32) -> u32 {
    let cell = u64::from(index) * u64::from(cells) / u64::from(extent);
    (cell as u32).min(cells - 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, path::PathBuf};

    fn fixture(name: &str) -> (Vec<u8>, u32, u32) {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/basic")
            .join(name);
        let value: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(path).expect("a readable fixture"))
                .expect("valid fixture JSON");
        (
            value["rgba"]
                .as_array()
                .expect("rgba")
                .iter()
                .map(|byte| byte.as_u64().expect("a byte") as u8)
                .collect(),
            value["width"].as_u64().expect("width") as u32,
            value["height"].as_u64().expect("height") as u32,
        )
    }

    #[test]
    fn an_isolated_clipped_pixel_survives_a_two_by_two_to_one_reduction() {
        let (rgba, width, height) = fixture("isolated-clip-2x2.json");
        assert_eq!((width, height), (2, 2));
        // One pixel has green at 255 among otherwise unclipped mid-greys.
        assert_eq!(
            overlay(&rgba, width, height, 1, 1).unwrap(),
            vec![OVERLAY_HIGHLIGHT],
            "a Fit cell keeps the one clipped sample instead of averaging it away"
        );
        // One to one: only that pixel's own cell lights up.
        assert_eq!(
            overlay(&rgba, width, height, 2, 2).unwrap(),
            vec![OVERLAY_NONE, OVERLAY_NONE, OVERLAY_HIGHLIGHT, OVERLAY_NONE]
        );
    }

    #[test]
    fn a_both_endpoint_pixel_reports_three_and_a_cell_grid_equal_to_the_image_is_one_to_one() {
        let (rgba, width, height) = fixture("both-endpoint-2x2.json");
        // [0,255,0] is both, [50,60,70] is none, [0,0,0] is shadow, [255,255,255] is highlight.
        assert_eq!(
            overlay(&rgba, width, height, 2, 2).unwrap(),
            vec![
                OVERLAY_BOTH,
                OVERLAY_NONE,
                OVERLAY_SHADOW,
                OVERLAY_HIGHLIGHT
            ]
        );
        assert_eq!(OVERLAY_BOTH, 3);
        // Reducing the whole image to one cell ORs every class together.
        assert_eq!(
            overlay(&rgba, width, height, 1, 1).unwrap(),
            vec![OVERLAY_BOTH]
        );
        // A shadow cell and a highlight cell that share a display cell also read `both`.
        assert_eq!(
            overlay(&rgba, width, height, 1, 2).unwrap(),
            vec![OVERLAY_BOTH, OVERLAY_BOTH]
        );
    }

    #[test]
    fn an_overlay_is_bounded_and_validates_its_inputs() {
        let rgba = vec![0u8; 4];
        assert_eq!(
            overlay(&rgba, 1, 1, MAX_OVERLAY_CELLS + 1, 1)
                .expect_err("beyond the cell bound")
                .kind,
            ErrorKind::ResourceLimit
        );
        assert_eq!(
            overlay(&rgba, 1, 1, 0, 1).expect_err("an empty grid").kind,
            ErrorKind::Validation
        );
        assert_eq!(
            overlay(&rgba, 2, 1, 1, 1)
                .expect_err("a buffer that is the wrong length")
                .kind,
            ErrorKind::Validation
        );
        // More cells than pixels still maps every pixel into exactly one cell.
        assert_eq!(overlay(&rgba, 1, 1, 3, 3).unwrap().len(), 9);
    }

    #[test]
    fn the_parallel_path_agrees_with_the_serial_one_on_a_megapixel_frame() {
        // One megapixel is the parallel threshold, so this frame takes the Rayon path; the same
        // pixels reduced by the serial path below must give the identical grid.
        let (width, height) = (1024u32, 1024u32);
        let mut rgba = vec![64u8; (width as usize) * (height as usize) * 4];
        for pixel in rgba.chunks_exact_mut(4) {
            pixel[3] = 255;
        }
        let clipped = |x: u32, y: u32| ((y as usize) * (width as usize) + x as usize) * 4;
        rgba[clipped(5, 7)] = 0;
        rgba[clipped(1000, 1000) + 1] = 255;
        assert!(u64::from(width) * u64::from(height) >= PARALLEL_REDUCE_PIXELS);
        let parallel = overlay(&rgba, width, height, 8, 8).unwrap();
        let mut serial = vec![OVERLAY_NONE; 64];
        for y in 0..height {
            let row = &rgba[(y as usize) * (width as usize) * 4..][..(width as usize) * 4];
            fold_row(&mut serial, row, y, width, height, 8, 8);
        }
        assert_eq!(parallel, serial);
        assert_eq!(parallel[0], OVERLAY_SHADOW, "the one dark sample");
        assert_eq!(parallel[63], OVERLAY_HIGHLIGHT, "the one bright sample");
        assert_eq!(
            parallel
                .iter()
                .filter(|cell| **cell != OVERLAY_NONE)
                .count(),
            2
        );
    }
}
