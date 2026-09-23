//! The mask coverage overlay: one mask's composed coverage as a bounded display-cell grid.
//!
//! The [masking design](../../../docs/design/masking.md)'s overlay is "one byte per display cell on
//! the grid the clipping overlay already defines and bounds", and this is that grid. It reuses
//! [`cell_pixel`](super::overlay::cell_pixel) — the delivered clipping overlay's own cell
//! arithmetic — and the delivered [`MAX_OVERLAY_CELLS`] bound rather than inventing a second grid,
//! so the two overlays address the same cells of the same frame and cannot drift apart.
//!
//! **Nothing full-resolution is allocated, at any stage size.** The clipping overlay must walk every
//! pixel, because one clipped pixel among millions has to survive a Fit reduction. A coverage field
//! has no such isolated feature to rescue: it is continuous, and the honest display-sized answer is
//! the field *sampled* once per cell. So this reduction is `O(cells × components)` and never
//! `O(pixels)`, it holds one byte per cell and nothing else, and the buffer is bounded by
//! [`MAX_OVERLAY_CELLS`] a side whatever the frame behind it is. That is the same rule
//! [`CompiledMask`] itself is written to — no mask plane is ever materialized ([performance rules 4
//! and 6](../../../docs/engineering/performance-rules.md#rules)) — restated for the display.
//!
//! **The grid describes the frame it arrived with.** A mask is compiled against the *content* stage
//! its layer receives, and the frame is the *output* stage after the geometry tail, so a cell is
//! answered by mapping its own output pixel's centre back through the tail's one affine
//! ([`StageTransform`]) and asking [`CompiledMask::coverage`] about the content pixel that lands in.
//! That is the same coordinate convention and the same rounding `render.locate` walks, so the
//! overlay and a pick agree about which content pixel an output pixel holds.
use super::overlay::{MAX_OVERLAY_CELLS, cell_pixel};
use crate::{
    Cancel, ComponentId, Error, ErrorKind, MaskId, StageTransform, mask::CompiledMask,
    modules::Stage,
};
use rayon::prelude::*;

/// A cell no part of the mask reaches.
pub const MASK_COVERAGE_NONE: u8 = 0;

/// The pixel value handed to a field that does not read one. `coverage_grid` refuses a mask whose
/// coverage depends on a pixel before any cell is filled, so this is never consulted by any
/// component; it exists because the field's signature takes a pixel and this grid has none.
const NO_PIXEL: [f64; 3] = [0.0, 0.0, 0.0];
/// A cell the mask covers completely.
pub const MASK_COVERAGE_FULL: u8 = 255;

/// The cell count above which the grid is filled on the shared Rayon pool. It is the same
/// one-megapixel threshold the reducer and the rasterizer use, counted in cells here because cells
/// are what this pass walks ([performance rule 9](../../../docs/engineering/performance-rules.md#rules)).
const PARALLEL_GRID_CELLS: u64 = super::PARALLEL_REDUCE_PIXELS;

/// One mask's coverage over one rendered frame: one byte per display cell, and the identity of what
/// the bytes describe.
///
/// `coverage` is exactly `cells_w * cells_h` bytes, row-major, addressed by the same cell rule the
/// clipping overlay uses. `component` names the one component this grid is the contribution of, or
/// `None` when it is the whole composed mask.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MaskOverlay {
    pub mask: MaskId,
    pub component: Option<ComponentId>,
    pub cells_w: u32,
    pub cells_h: u32,
    pub coverage: Vec<u8>,
}

/// Coverage in `[0, 1]` as one byte: `(coverage · 255).round()`.
///
/// Public because the consumer that paints the grid has to agree with the worker that filled it
/// about what a byte means, and because a test computing the expected grid from [`CompiledMask`]
/// must quantize the same way. Both ends of the field are *reached*, not approached: coverage `0.0`
/// is exactly [`MASK_COVERAGE_NONE`] and coverage `1.0` exactly [`MASK_COVERAGE_FULL`], so a cell
/// the mask does not touch and a cell it covers completely are never off by a code.
pub fn quantize_coverage(coverage: f64) -> u8 {
    (coverage.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// The content pixel an output coordinate lands in, or `None` when it lands outside the content
/// stage.
///
/// A fitted straightening crop covers the whole rotated bounding box, so an output pixel can map
/// outside the picture; there is no content there to mask, and clamping to the edge pixel would
/// report a coverage the recipe never puts there. Such a cell reads [`MASK_COVERAGE_NONE`].
fn content_pixel(coordinate: f64, extent: u32) -> Option<u32> {
    if !coordinate.is_finite() {
        return None;
    }
    let index = coordinate.floor();
    if index < 0.0 || index >= f64::from(extent) {
        return None;
    }
    Some(index as u32)
}

/// Everything one cell row needs that does not depend on which row it is: the two stages, the
/// output-to-content affine and the grid's shape. Computed once per grid, never per cell.
#[derive(Clone, Copy)]
struct Cells {
    content: Stage,
    output: Stage,
    inverse: [f64; 6],
    cells_w: u32,
    cells_h: u32,
}

impl Cells {
    /// Fill one cell row. `row` is that row's slice of the grid and `cy` its cell row index.
    fn fill_row(&self, row: &mut [u8], cy: u32, mask: &CompiledMask) {
        let py = cell_pixel(cy, self.output.height, self.cells_h);
        let oy = f64::from(py) + 0.5;
        for (cx, cell) in row.iter_mut().enumerate() {
            let px = cell_pixel(cx as u32, self.output.width, self.cells_w);
            let ox = f64::from(px) + 0.5;
            let x = self.inverse[0] * ox + self.inverse[1] * oy + self.inverse[2];
            let y = self.inverse[3] * ox + self.inverse[4] * oy + self.inverse[5];
            *cell = match (
                content_pixel(x, self.content.width),
                content_pixel(y, self.content.height),
            ) {
                // Every mask that reaches this grid is position-only, which `coverage_grid` has
                // already established, so the pixel value the field takes is never read and the
                // neutral triple below stands for "no pixel was consulted" rather than for a colour.
                (Some(x), Some(y)) => quantize_coverage(mask.coverage(x, y, NO_PIXEL)),
                _ => MASK_COVERAGE_NONE,
            };
        }
    }
}

/// One compiled mask's coverage over the frame `transform` describes, as a `cells_w × cells_h` grid
/// of quantized bytes — or `None` when there is nothing to describe.
///
/// **Absent rather than empty.** A mask whose coverage is exactly zero at every pixel of its stage
/// — an amount of zero, or a mask with no components — answers `None`, never a grid of zeros that a
/// reader could mistake for a real, fully uncovered selection. That is the histogram contract's "no
/// silently empty result" rule, and it is decided from [`CompiledMask::bounds`], which is closed
/// form and `O(components)`, so deciding it costs no cell at all. A mask whose selection the crop
/// happens to exclude is a different thing: it *is* a mask of this frame and its honest grid over
/// this frame is zeros, so it is present.
///
/// Cost is `O(cells × components)` with one byte of state per cell. It reads no pixel of the frame
/// and allocates nothing proportional to the stage, and it is bounded by [`MAX_OVERLAY_CELLS`] a
/// side. Serial below a megapixel of cells and on the shared Rayon pool above it; the cells are
/// independent, so the split decides nothing about the result. `cancel` is read once per cell row.
pub fn coverage_grid(
    mask: &CompiledMask,
    transform: &StageTransform,
    cells_w: u32,
    cells_h: u32,
    cancel: &Cancel,
) -> Result<Option<Vec<u8>>, Error> {
    cancel.check()?;
    let output = Stage {
        width: transform.output.width,
        height: transform.output.height,
    };
    let content = Stage {
        width: transform.content.width,
        height: transform.content.height,
    };
    if output.width == 0 || output.height == 0 || cells_w == 0 || cells_h == 0 {
        return Err(Error::new(
            ErrorKind::Validation,
            "a mask overlay needs a non-empty frame and a non-empty cell grid",
        ));
    }
    if cells_w > MAX_OVERLAY_CELLS || cells_h > MAX_OVERLAY_CELLS {
        return Err(Error::new(
            ErrorKind::ResourceLimit,
            format!(
                "a mask overlay of {cells_w}x{cells_h} cells exceeds the {MAX_OVERLAY_CELLS} cells a side the display overlay allows"
            ),
        ));
    }
    // The mask answers about the stage it was compiled against, and `transform` maps that same
    // stage onto the frame. Two different stages would be two different coverage fields, so the
    // mismatch is refused rather than resolved by scaling one of them.
    if mask.stage() != content {
        return Err(Error::new(
            ErrorKind::Validation,
            format!(
                "the mask was compiled against a {}x{} stage and this frame's content stage is {}x{}",
                mask.stage().width,
                mask.stage().height,
                content.width,
                content.height
            ),
        ));
    }
    if mask.bounds().is_empty() {
        return Ok(None);
    }
    // A value-based component's coverage is a function of the pixel the masked *operation* receives,
    // and this grid is a function of position over the finished frame: the frame's own pixel is that
    // operation's output and not its input, so painting it here would draw a selection the render
    // never makes. The honest answer is that there is no grid, with the reason named, until the
    // overlay is given the masked operation's input — which is its own decision (proposal P16 of
    // `docs/design/range-study.md`) and not something to guess at per cell.
    if mask.reads_pixels() {
        return Err(Error::new(
            ErrorKind::Validation,
            "this mask has a component whose coverage depends on the pixel it reads, and a \
             coverage grid is a function of position over the finished frame; the 100% \
             view is where such a selection can be read",
        ));
    }
    let count = (cells_w as usize) * (cells_h as usize);
    let cells = Cells {
        content,
        output,
        inverse: transform.inverse,
        cells_w,
        cells_h,
    };
    let mut grid = vec![MASK_COVERAGE_NONE; count];
    let row = cells_w as usize;
    if count as u64 >= PARALLEL_GRID_CELLS {
        grid.par_chunks_mut(row)
            .enumerate()
            .try_for_each(|(cy, slice)| {
                cancel.check()?;
                cells.fill_row(slice, cy as u32, mask);
                Ok::<(), Error>(())
            })?;
    } else {
        for (cy, slice) in grid.chunks_mut(row).enumerate() {
            cancel.check()?;
            cells.fill_row(slice, cy as u32, mask);
        }
    }
    Ok(Some(grid))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Component, ComponentMode, Mask, StageSize};
    use serde_json::json;

    /// The pixel value a geometric component is handed and ignores (proposal P12 of
    /// `docs/design/range-study.md`): the masks here hold gradients and their coverage is a
    /// function of position alone, so the value is arbitrary and the same at every call.
    const ANY_PIXEL: [f64; 3] = [0.25, 0.5, 0.75];

    /// The identity tail: a frame that is its content stage, which is what a recipe with no
    /// geometry layer produces.
    fn identity(width: u32, height: u32) -> StageTransform {
        StageTransform {
            content: StageSize { width, height },
            output: StageSize { width, height },
            forward: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            inverse: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
        }
    }

    fn vertical_gradient() -> Mask {
        let mut mask = Mask::new("Mask 1");
        mask.components.push(Component::new(
            "Linear 1",
            ComponentMode::Add,
            "linear",
            json!({"x0": 0.5, "y0": 0.0, "x1": 0.5, "y1": 1.0}),
        ));
        mask
    }

    fn compiled(mask: &Mask, width: u32, height: u32) -> CompiledMask {
        CompiledMask::new(
            mask,
            Stage { width, height },
            &crate::path::StrokeTable::default(),
        )
        .expect("the gradient compiles")
    }

    #[test]
    fn quantization_reaches_both_ends_of_the_field() {
        assert_eq!(quantize_coverage(0.0), MASK_COVERAGE_NONE);
        assert_eq!(quantize_coverage(1.0), MASK_COVERAGE_FULL);
        assert_eq!(quantize_coverage(0.5), 128);
        // Out of range and not-a-number never index past a byte.
        assert_eq!(quantize_coverage(-1.0), MASK_COVERAGE_NONE);
        assert_eq!(quantize_coverage(2.0), MASK_COVERAGE_FULL);
        assert_eq!(quantize_coverage(f64::NAN), MASK_COVERAGE_NONE);
    }

    /// Every byte is the quantized coverage of the content pixel that cell's own output pixel holds.
    /// The expected value is computed from [`CompiledMask`] directly, through a second transcription
    /// of the cell arithmetic written out here rather than shared with the unit under test.
    #[test]
    fn every_cell_is_the_quantized_coverage_of_the_pixel_it_represents() {
        let mask = vertical_gradient();
        let (width, height) = (200, 120);
        let compiled = compiled(&mask, width, height);
        let transform = identity(width, height);
        let (cells_w, cells_h) = (37, 23);
        let grid = coverage_grid(&compiled, &transform, cells_w, cells_h, &Cancel::never())
            .expect("a grid")
            .expect("the gradient covers part of the stage");
        assert_eq!(grid.len(), (cells_w * cells_h) as usize);
        for cy in 0..cells_h {
            // The centre of this cell's own pixel span, transcribed independently.
            let py =
                (((2 * u64::from(cy) + 1) * u64::from(height)) / (2 * u64::from(cells_h))) as u32;
            for cx in 0..cells_w {
                let px = (((2 * u64::from(cx) + 1) * u64::from(width)) / (2 * u64::from(cells_w)))
                    as u32;
                assert_eq!(
                    grid[(cy * cells_w + cx) as usize],
                    quantize_coverage(compiled.coverage(px, py, ANY_PIXEL)),
                    "cell ({cx}, {cy}) over pixel ({px}, {py})"
                );
            }
        }
        // The gradient runs top to bottom, so the first cell row is uncovered and the last is full.
        assert_eq!(grid[0], MASK_COVERAGE_NONE);
        assert_eq!(grid[grid.len() - 1], MASK_COVERAGE_FULL);
    }

    /// The forced-parallel and forced-serial paths cannot be compared through the public entry
    /// point without a megapixel of cells, so this compares the real grid against the same row
    /// function driven serially, which is what the parallel split is a rearrangement of.
    #[test]
    fn the_cell_rows_are_independent_of_how_they_are_split() {
        let mask = vertical_gradient();
        let (width, height) = (640, 480);
        let compiled = compiled(&mask, width, height);
        let transform = identity(width, height);
        let (cells_w, cells_h) = (128, 96);
        let grid = coverage_grid(&compiled, &transform, cells_w, cells_h, &Cancel::never())
            .unwrap()
            .unwrap();
        let cells = Cells {
            content: Stage { width, height },
            output: Stage { width, height },
            inverse: transform.inverse,
            cells_w,
            cells_h,
        };
        let mut serial = vec![MASK_COVERAGE_NONE; (cells_w * cells_h) as usize];
        for (cy, slice) in serial.chunks_mut(cells_w as usize).enumerate() {
            cells.fill_row(slice, cy as u32, &compiled);
        }
        assert_eq!(grid, serial);
    }

    /// Nothing to describe is absent, never a grid of zeros. A mask with no components and a mask
    /// at amount zero are both exactly zero everywhere, which is what `bounds` reports in closed
    /// form; a mask the crop excludes is a real mask of this frame and keeps its zeros.
    #[test]
    fn a_mask_with_nothing_to_describe_has_no_grid_at_all() {
        let (width, height) = (64, 48);
        let transform = identity(width, height);

        let empty = Mask::new("Mask 1");
        assert_eq!(
            coverage_grid(
                &compiled(&empty, width, height),
                &transform,
                8,
                8,
                &Cancel::never()
            )
            .unwrap(),
            None,
            "a mask with no components selects nothing and describes nothing"
        );

        let mut silent = vertical_gradient();
        silent.amount = 0.0;
        assert_eq!(
            coverage_grid(
                &compiled(&silent, width, height),
                &transform,
                8,
                8,
                &Cancel::never()
            )
            .unwrap(),
            None,
            "an amount of zero multiplies the whole field to zero"
        );

        // The same gradient at a non-zero amount is present, and its bytes are attenuated rather
        // than absent: an amount is a multiplier on a field that is still there.
        let mut quiet = vertical_gradient();
        quiet.amount = 50.0;
        let half = coverage_grid(
            &compiled(&quiet, width, height),
            &transform,
            8,
            8,
            &Cancel::never(),
        )
        .unwrap()
        .expect("half an amount still describes something");
        let full = coverage_grid(
            &compiled(&vertical_gradient(), width, height),
            &transform,
            8,
            8,
            &Cancel::never(),
        )
        .unwrap()
        .unwrap();
        let (quietest, loudest) = (*half.iter().max().unwrap(), *full.iter().max().unwrap());
        assert!(
            quietest < loudest && quietest > loudest / 3,
            "half an amount is half a field, not none and not all: {quietest} against {loudest}"
        );
    }

    /// The delivered cell cap bounds the grid however large the stage behind it is, and the
    /// refusal names the bound. 5000 px a side is past the cap, so a caller asking for a cell per
    /// pixel is refused and a caller asking for the cap gets exactly the cap.
    #[test]
    fn the_grid_is_bounded_by_the_delivered_cell_cap_on_an_oversized_stage() {
        let (width, height) = (5000_u32, 4200_u32);
        let mask = vertical_gradient();
        let compiled = compiled(&mask, width, height);
        let transform = identity(width, height);

        let error = coverage_grid(&compiled, &transform, width, 8, &Cancel::never())
            .expect_err("a cell per pixel is past the cap");
        assert_eq!(error.kind, ErrorKind::ResourceLimit);
        assert!(
            error.detail.contains(&MAX_OVERLAY_CELLS.to_string()),
            "the refusal names the bound: {}",
            error.detail
        );

        let grid = coverage_grid(
            &compiled,
            &transform,
            MAX_OVERLAY_CELLS,
            8,
            &Cancel::never(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(grid.len(), MAX_OVERLAY_CELLS as usize * 8);
        // One byte per cell, bounded by the cap a side: the buffer never scales with the stage.
        assert!(grid.len() < (width as usize) * (height as usize));
    }

    #[test]
    fn a_grid_validates_its_frame_and_its_stage() {
        let (width, height) = (64, 48);
        let mask = vertical_gradient();
        let compiled = compiled(&mask, width, height);
        assert_eq!(
            coverage_grid(&compiled, &identity(width, height), 0, 8, &Cancel::never())
                .expect_err("an empty grid")
                .kind,
            ErrorKind::Validation
        );
        // A transform describing another content stage is two different coverage fields.
        let elsewhere = identity(width + 1, height);
        let error = coverage_grid(&compiled, &elsewhere, 8, 8, &Cancel::never())
            .expect_err("a stage the mask was not compiled against");
        assert_eq!(error.kind, ErrorKind::Validation);
        assert!(error.detail.contains("64x48"), "{}", error.detail);
    }

    #[test]
    fn a_cancelled_token_stops_the_grid() {
        let mask = vertical_gradient();
        let compiled = compiled(&mask, 64, 48);
        let cancel = Cancel::new();
        cancel.cancel();
        assert_eq!(
            coverage_grid(&compiled, &identity(64, 48), 8, 8, &cancel)
                .expect_err("a cancelled token refuses the grid")
                .kind,
            ErrorKind::Cancelled
        );
    }

    /// An output pixel the geometry tail maps outside the content stage has no picture behind it,
    /// so its cell is uncovered rather than clamped onto the nearest content pixel.
    #[test]
    fn a_cell_outside_the_content_stage_is_uncovered() {
        let (width, height) = (64, 48);
        let mask = vertical_gradient();
        let compiled = compiled(&mask, width, height);
        // A frame twice as tall as the content, sitting over it from the top: the bottom half of
        // every column maps past the content stage's last row.
        let transform = StageTransform {
            content: StageSize { width, height },
            output: StageSize {
                width,
                height: height * 2,
            },
            forward: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            inverse: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
        };
        let (cells_w, cells_h) = (4, 4);
        let grid = coverage_grid(&compiled, &transform, cells_w, cells_h, &Cancel::never())
            .unwrap()
            .unwrap();
        // Rows 0 and 1 sample output rows 12 and 36, which are on the picture; rows 2 and 3 sample
        // 60 and 84, which are past its last row. The zeros are the frame running out of content,
        // not the mask running out of coverage.
        for cy in 0..cells_h {
            let on_picture = cell_pixel(cy, transform.output.height, cells_h) < height;
            for cx in 0..cells_w {
                let cell = grid[(cy * cells_w + cx) as usize];
                if on_picture {
                    assert!(
                        cell > MASK_COVERAGE_NONE,
                        "cell ({cx}, {cy}) is on the picture and inside the gradient"
                    );
                } else {
                    assert_eq!(
                        cell, MASK_COVERAGE_NONE,
                        "cell ({cx}, {cy}) is off the picture"
                    );
                }
            }
        }
    }
    /// A mask with a value-based component has no coverage grid, and the refusal says why.
    ///
    /// The grid is a function of position over the *finished* frame; a range selection's coverage is
    /// a function of the pixel the masked **operation** receives, which is that frame's input and
    /// not its output. Painting the frame's own pixel here would draw a selection the render never
    /// makes, so the honest answer is a refusal naming the reason, and where such a selection can be
    /// read — the 100% view. Giving the overlay the masked operation's input is proposal P16 of
    /// `docs/design/range-study.md` and a decision of its own.
    #[test]
    fn a_mask_that_reads_pixels_has_no_coverage_grid() {
        let (width, height) = (64, 48);
        let mut mask = vertical_gradient();
        let name = mask.next_component_name("luminance-range");
        mask.components.push(Component::new(
            name,
            ComponentMode::Intersect,
            "luminance-range",
            json!({"low": 20.0, "low_feather": 10.0, "high": 80.0, "high_feather": 10.0}),
        ));
        let mixed = compiled(&mask, width, height);
        assert!(mixed.reads_pixels());
        let error = coverage_grid(&mixed, &identity(width, height), 8, 8, &Cancel::never())
            .expect_err("a value-based mask has no grid");
        assert_eq!(error.kind, ErrorKind::Validation);
        assert!(
            error.detail.contains("100% view"),
            "the refusal says where the selection can be read: {}",
            error.detail
        );
        // The geometric half of the same mask still has one, so a client can show the component
        // whose contribution a grid *can* describe.
        let geometry = compiled(&vertical_gradient(), width, height);
        assert!(!geometry.reads_pixels());
        assert!(
            coverage_grid(&geometry, &identity(width, height), 8, 8, &Cancel::never())
                .unwrap()
                .is_some()
        );
    }
}
