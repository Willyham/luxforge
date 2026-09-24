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
/// Serial below one megapixel and on the shared Rayon pool above it, with disjoint output rows.
/// Only the final cell grid is allocated, bounded by [`MAX_OVERLAY_CELLS`] a side.
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
    let mut grid = vec![OVERLAY_NONE; cells];
    if pixels >= PARALLEL_REDUCE_PIXELS {
        // A cell row owns exactly the source rows y for which floor(y * cells_h / height)
        // equals its index. Inverting that interval with ceiling division gives disjoint spans,
        // including empty spans when the grid has more rows than the image. Every source row is
        // read once and writes only this output slice: no private grids or OR merges are needed.
        grid.par_chunks_mut(cells_w as usize)
            .enumerate()
            .for_each(|(cell_y, cells)| {
                let first =
                    (cell_y as u64 * u64::from(height)).div_ceil(u64::from(cells_h)) as usize;
                let end =
                    ((cell_y as u64 + 1) * u64::from(height)).div_ceil(u64::from(cells_h)) as usize;
                for row in rgba[first * row_bytes..end * row_bytes].chunks_exact(row_bytes) {
                    fold_row(cells, row, width, cells_w);
                }
            });
    } else {
        for (y, row) in rgba.chunks_exact(row_bytes).enumerate() {
            let base = cell_index(y as u32, height, cells_h) as usize * cells_w as usize;
            fold_row(
                &mut grid[base..base + cells_w as usize],
                row,
                width,
                cells_w,
            );
        }
    }
    Ok(grid)
}

/// OR one source row into its output cell row.
fn fold_row(cells: &mut [u8], row: &[u8], width: u32, cells_w: u32) {
    for (x, pixel) in row.chunks_exact(4).enumerate() {
        let class = bits(clip_class([pixel[0], pixel[1], pixel[2], pixel[3]]));
        if class == OVERLAY_NONE {
            continue;
        }
        let cell_x = cell_index(x as u32, width, cells_w) as usize;
        cells[cell_x] |= class;
    }
}

/// `floor(index * cells / extent)`, computed in 64 bits and clamped to the last cell so a rounding
/// edge can never index past the grid.
pub(crate) fn cell_index(index: u32, extent: u32, cells: u32) -> u32 {
    let cell = u64::from(index) * u64::from(cells) / u64::from(extent);
    (cell as u32).min(cells - 1)
}

/// The source pixel one display cell is represented *by*: `floor((2·cell + 1)·extent / (2·cells))`,
/// the pixel at the centre of that cell's own span, computed in 64 bits and clamped to the last
/// pixel.
///
/// This is [`cell_index`] read the other way, and the two agree exactly: while a cell covers at
/// least one pixel, `cell_index(cell_pixel(c, e, n), e, n) == c` for every cell of every grid, which
/// this module's tests assert over the whole cell cap rather than on an example.
///
/// Two overlays need the same grid for different reasons. A clipping overlay *folds* pixels into
/// cells, because a single clipped pixel must survive a Fit reduction, so it walks the frame and
/// costs `O(pixels)`. A coverage field is continuous and needs no such rescue, so a mask overlay
/// *samples* one pixel per cell instead and costs `O(cells)` — display-sized work for a
/// display-sized answer, with no full-resolution mask plane anywhere. Both address cells through
/// this one arithmetic, so the two grids land on the same cells over the same frame and cannot
/// drift apart.
pub(crate) fn cell_pixel(cell: u32, extent: u32, cells: u32) -> u32 {
    let pixel = (2 * u64::from(cell) + 1) * u64::from(extent) / (2 * u64::from(cells));
    (pixel as u32).min(extent - 1)
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

    /// Independent source-pixel traversal: no output-row spans, production fold or cell helpers.
    fn serial_oracle(rgba: &[u8], width: u32, height: u32, cells_w: u32, cells_h: u32) -> Vec<u8> {
        let mut cells = vec![0; cells_w as usize * cells_h as usize];
        for (index, pixel) in rgba.chunks_exact(4).enumerate() {
            let x = index as u64 % u64::from(width);
            let y = index as u64 / u64::from(width);
            let cell_x = x * u64::from(cells_w) / u64::from(width);
            let cell_y = y * u64::from(cells_h) / u64::from(height);
            let bits =
                u8::from(pixel[..3].contains(&0)) | (u8::from(pixel[..3].contains(&255)) << 1);
            cells[(cell_y * u64::from(cells_w) + cell_x) as usize] |= bits;
        }
        cells
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

    /// The two halves of the cell arithmetic are exact inverses of each other wherever a cell
    /// covers at least one pixel: the pixel a cell is represented by falls in that same cell. The
    /// mask overlay samples that pixel and the clipping overlay folds it in, so were the two ever
    /// to disagree the two overlays would describe different cells of the same frame.
    #[test]
    fn a_cells_representative_pixel_falls_in_that_cell() {
        for extent in [1u32, 2, 3, 7, 64, 1000, 4095, 4096, 16384] {
            for cells in [1u32, 2, 3, 8, 97, 1024, MAX_OVERLAY_CELLS] {
                if cells > extent {
                    // More cells than pixels: several cells share one pixel, so the map is not
                    // injective and only the clamp is claimed.
                    for cell in [0, cells / 2, cells - 1] {
                        assert!(cell_pixel(cell, extent, cells) < extent);
                    }
                    continue;
                }
                for cell in 0..cells {
                    let pixel = cell_pixel(cell, extent, cells);
                    assert!(
                        pixel < extent,
                        "{extent}/{cells}: cell {cell} names {pixel}"
                    );
                    assert_eq!(
                        cell_index(pixel, extent, cells),
                        cell,
                        "{extent} pixels into {cells} cells: cell {cell} names pixel {pixel}"
                    );
                }
            }
        }
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
        let serial = serial_oracle(&rgba, width, height, 8, 8);
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

    #[test]
    fn disjoint_rows_match_the_oracle_for_partial_empty_and_capped_cell_rows() {
        // The large frame enters the parallel path with nondivisible dimensions. The small one
        // also checks the serial path; enlarged grids have output rows with no source pixels.
        for (width, height) in [(13u32, 7u32), (1031, 1019)] {
            let mut rgba = vec![0; width as usize * height as usize * 4];
            for (index, pixel) in rgba.chunks_exact_mut(4).enumerate() {
                let x = index as u32 % width;
                let y = index as u32 / width;
                // Include dense clipping, unclipped pixels and both endpoints. Alpha alternates
                // endpoints independently, so consulting it would change clean cells.
                let palette = [[60, 90, 120], [0, 80, 100], [70, 255, 100], [0, 90, 255]];
                pixel[..3].copy_from_slice(&palette[((x * 17 + y * 7) % 29 / 8) as usize]);
                pixel[3] = if index % 2 == 0 { 0 } else { 255 };
            }
            for (cells_w, cells_h) in [
                (1, 1),
                (1, 13),
                (17, 1),
                (37, 29),
                (1023, 1009),
                (width, height),
                (MAX_OVERLAY_CELLS, MAX_OVERLAY_CELLS),
            ] {
                assert_eq!(
                    overlay(&rgba, width, height, cells_w, cells_h).unwrap(),
                    serial_oracle(&rgba, width, height, cells_w, cells_h),
                    "{width}x{height} into {cells_w}x{cells_h}"
                );
            }
        }
    }

    #[test]
    fn isolated_clips_at_nondivisible_row_boundaries_reach_only_their_own_cells() {
        let (width, height, cells_w, cells_h) = (1031u32, 1019u32, 37u32, 29u32);
        let mut rgba = vec![64; width as usize * height as usize * 4];
        let set = |rgba: &mut [u8], x: u32, y: u32, pixel: [u8; 4]| {
            let index = (y as usize * width as usize + x as usize) * 4;
            rgba[index..index + 4].copy_from_slice(&pixel);
        };
        // The first row of every cell row and the row immediately before it; floor mapping in
        // the oracle decides ownership independently of the production interval calculation.
        for cell_y in 1..cells_h {
            let boundary =
                (u64::from(cell_y) * u64::from(height)).div_ceil(u64::from(cells_h)) as u32;
            set(&mut rgba, 0, boundary - 1, [0, 64, 64, 255]);
            set(&mut rgba, width - 1, boundary, [64, 64, 255, 0]);
        }
        set(&mut rgba, 0, 0, [0, 255, 64, 255]);
        set(&mut rgba, width - 1, height - 1, [0, 255, 64, 255]);
        let actual = overlay(&rgba, width, height, cells_w, cells_h).unwrap();
        assert_eq!(
            actual,
            serial_oracle(&rgba, width, height, cells_w, cells_h)
        );
        assert_eq!(actual[0], OVERLAY_BOTH);
        assert_eq!(actual[actual.len() - 1], OVERLAY_BOTH);
    }
}
