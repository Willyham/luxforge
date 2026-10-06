//! A lens warp's coordinate grid: the geometry tail's output-to-boundary map sampled at regular
//! output nodes, which the surface interpolates bilinearly for every output pixel instead of
//! evaluating the warp's steps. A perspective warp with no lens needs none: its map is a
//! homography, which the surface evaluates exactly (`GpuGeometry::projective`).
//!
//! The density is chosen per grid, from the map itself: bilinear interpolation of a smooth map errs
//! by about `h²/8` of its second derivative across a cell of side `h`, largest near a cell's centre
//! and the middle of its edges, so the grid is built coarse, its error measured at every cell centre
//! and edge midpoint, and the spacing narrowed until that error is within half the contract. The
//! error is the displacement in the output stage of the content the interpolated coordinate reads,
//! `J⁻¹·δ` for the map's local Jacobian `J` and the interpolation error `δ` in boundary texels, and
//! the contract has two parts:
//!
//! - **Where it is seen**: [`GRID_TOLERANCE_PX`] display pixels, which a stage drawn magnified
//!   divides by the magnification.
//! - **What it reads**: [`GRID_SAMPLE_TOLERANCE_PX`] output pixels at any magnification. A
//!   displacement too small to see still moves the value a high-frequency source gives the pixel:
//!   on the zone plate, whose chirp reaches half a cycle a pixel, a lens warp's grid at the display
//!   bound alone (about 0.04 px) leaves a worst 16 × 16 block of 0.98 ΔE00 at 100%, against the
//!   pointwise limit of 1.0, and a perspective warp's 1.15 at Fit.
//!
//! The tighter of the two holds: the second at Fit and 100%, the first from a fourfold zoom.
//!
//! The grid is a function of the output region and the map, never of the image: it covers the
//! output the surface draws, which the display bounds (4096 px a side, 8 megapixels) or the region
//! frame bound (8 megapixels at 100%) already limit, and it holds at most [`GRID_MAX_NODES`] nodes.
//! It is computed once per draft, since a colour draft does not change the geometry.
use crate::{Error, modules::Region, render::map::GeometryMap};

/// How far, in display pixels, the content a grid's interpolated coordinate reads may lie from
/// where the exact map puts it.
pub const GRID_TOLERANCE_PX: f64 = 0.1;

/// How far, in output pixels, the content a grid's interpolated coordinate reads may lie from where
/// the exact map puts it, whatever the magnification: what a source with detail up to its own
/// Nyquist limit needs for the value each pixel samples to stay within the pointwise limits.
pub const GRID_SAMPLE_TOLERANCE_PX: f64 = 0.025;

/// The most nodes one grid holds: 2 MiB of `[f32; 2]`. A map that needs more for the tolerance
/// over the requested region takes the CPU path.
pub const GRID_MAX_NODES: usize = 1 << 18;

/// The error a spacing's check points may show, as a fraction of the tolerance: the check points
/// sit where bilinear interpolation errs most, and the half to spare covers the rest of the cell.
const ESTIMATE_FRACTION: f64 = 0.5;

/// The spacing a grid is first tried at, and the finest it may take, in output pixels.
const COARSEST: u32 = 64;
const FINEST: u32 = 2;

/// Output nodes and the boundary coordinate the map gives each.
#[derive(Clone, Debug, PartialEq)]
pub struct CoordinateGrid {
    /// The output pixel at which node `(0, 0)` sits: node `(i, j)` is at the pixel-edge coordinate
    /// `(origin.0 + i·spacing, origin.1 + j·spacing)` of the output stage.
    pub origin: (u32, u32),
    /// The distance between neighbouring nodes, in output pixels.
    pub spacing: u32,
    pub columns: u32,
    pub rows: u32,
    /// The boundary coordinate of every node, row by row, in pixel-edge coordinates. The last
    /// column and row may lie past the region's edge, so every pixel centre in it has four nodes.
    pub nodes: Vec<[f32; 2]>,
}

impl CoordinateGrid {
    /// The grid of `map` over `region` of its output stage, drawn at `magnification` display pixels
    /// per output pixel: within [`GRID_TOLERANCE_PX`] display pixels and
    /// [`GRID_SAMPLE_TOLERANCE_PX`] output pixels everywhere in the region. At Fit and at 100% the
    /// output stage is drawn at one display pixel per output pixel or fewer, so `magnification` is
    /// `1`; a stage drawn more than four times larger asks for a denser grid.
    ///
    /// Refused when the region lies outside the output stage, the map reaches a non-finite
    /// coordinate, or the tolerance needs more than [`GRID_MAX_NODES`] nodes.
    pub fn new(map: &GeometryMap, region: Region, magnification: f64) -> Result<Self, Error> {
        if region.is_empty() || region.x1() > map.output.width || region.y1() > map.output.height {
            return Err(Error::validation(format!(
                "a coordinate grid's region {region:?} is outside the {}x{} output stage",
                map.output.width, map.output.height
            )));
        }
        if !magnification.is_finite() || magnification <= 0.0 {
            return Err(Error::validation(
                "a coordinate grid's magnification must be positive",
            ));
        }
        let target = ESTIMATE_FRACTION
            * GRID_SAMPLE_TOLERANCE_PX.min(GRID_TOLERANCE_PX / magnification.max(1.0));
        let mut spacing = COARSEST;
        loop {
            let grid = Self::at(map, region, spacing)?;
            let error = grid.estimate(map, region);
            if !error.is_finite() {
                return Err(Error::render(
                    "a warp's coordinate grid reaches a degenerate mapping",
                ));
            }
            if error <= target {
                return Ok(grid);
            }
            if spacing == FINEST {
                return Err(Error::resource_limit(format!(
                    "a warp needs a coordinate grid finer than {FINEST} px"
                )));
            }
            // The error grows with the square of the spacing: aim for the spacing that meets the
            // target with a tenth to spare, and always refine.
            let aim = f64::from(spacing) * (target / error).sqrt() * 0.9;
            spacing = (aim.floor() as u32).clamp(FINEST, spacing - 1);
        }
    }

    /// The grid of `map` over its whole output stage at `magnification`, its spacing chosen once for
    /// the stage ([`Self::new`]): what every window of the stage takes its part of ([`Self::part`]),
    /// so a tile, a region and the whole frame interpolate the same nodes wherever they overlap and
    /// draw the same output pixel alike.
    pub fn stage(map: &GeometryMap, magnification: f64) -> Result<Self, Error> {
        Self::new(
            map,
            Region {
                x0: 0,
                y0: 0,
                width: map.output.width,
                height: map.output.height,
            },
            magnification,
        )
    }

    /// The part of this grid that covers `region` of the output stage: its nodes from the last at
    /// or before the region's origin to the first at or past its far edges, at the same spacing and
    /// with the same values, so every pixel centre in the region interpolates the nodes the whole
    /// grid gives it. `None` when the region is empty or reaches past the grid's nodes. `O(nodes
    /// of the part)`.
    pub fn part(&self, region: Region) -> Option<Self> {
        if region.is_empty() || region.x0 < self.origin.0 || region.y0 < self.origin.1 {
            return None;
        }
        let axis = |from: u32, to: u32, origin: u32, count: u32| {
            let first = (from - origin) / self.spacing;
            // The node at or past the far edge, and at least one after the first.
            let last = (to - origin).div_ceil(self.spacing).max(first + 1);
            (last < count).then_some((first, last - first + 1))
        };
        let (column, columns) = axis(region.x0, region.x1(), self.origin.0, self.columns)?;
        let (row, rows) = axis(region.y0, region.y1(), self.origin.1, self.rows)?;
        let mut nodes = Vec::with_capacity(columns as usize * rows as usize);
        for y in row..row + rows {
            let start = (y * self.columns + column) as usize;
            nodes.extend_from_slice(&self.nodes[start..start + columns as usize]);
        }
        Some(Self {
            origin: (
                self.origin.0 + column * self.spacing,
                self.origin.1 + row * self.spacing,
            ),
            spacing: self.spacing,
            columns,
            rows,
            nodes,
        })
    }

    /// The grid at one spacing: its nodes on the stage's lattice of that spacing, from the last
    /// at or before the region's origin to the first at or past its far edges, so a node is
    /// where it is in every grid of that spacing.
    fn at(map: &GeometryMap, region: Region, spacing: u32) -> Result<Self, Error> {
        let (first_column, first_row) = (region.x0 / spacing, region.y0 / spacing);
        let columns = region.x1().div_ceil(spacing).max(first_column + 1) - first_column + 1;
        let rows = region.y1().div_ceil(spacing).max(first_row + 1) - first_row + 1;
        let region = Region {
            x0: first_column * spacing,
            y0: first_row * spacing,
            ..region
        };
        let count = u64::from(columns) * u64::from(rows);
        if count > GRID_MAX_NODES as u64 {
            return Err(Error::resource_limit(format!(
                "a warp's coordinate grid needs {count} nodes at {spacing} px, more than the \
                 {GRID_MAX_NODES} one grid holds"
            )));
        }
        let mut nodes = Vec::with_capacity(count as usize);
        for row in 0..rows {
            for column in 0..columns {
                let (u, v) = map.content_at(
                    f64::from(region.x0) + f64::from(column * spacing),
                    f64::from(region.y0) + f64::from(row * spacing),
                );
                let node = [u as f32, v as f32];
                if !node.iter().all(|value| value.is_finite()) {
                    return Err(Error::render(
                        "a warp's coordinate grid reaches a non-finite coordinate",
                    ));
                }
                nodes.push(node);
            }
        }
        Ok(Self {
            origin: (region.x0, region.y0),
            spacing,
            columns,
            rows,
            nodes,
        })
    }

    fn node(&self, column: u32, row: u32) -> [f32; 2] {
        self.nodes[(row * self.columns + column) as usize]
    }

    /// The cell holding the output pixel-edge coordinate `at` on an axis of `count` nodes from
    /// lattice node `first`, and the fraction across it: the cell is the stage pixel's, in
    /// integers, and the fraction its offset from the cell's node over the spacing, both from the
    /// stage's own lattice and the coordinate alone, never from where the grid starts, so every
    /// grid of the spacing that holds the cell interpolates a pixel to the same bits.
    fn cell(at: f32, spacing: u32, first: u32, count: u32) -> (u32, f32) {
        let pixel = at.max(0.0).floor() as u32;
        let index = ((pixel / spacing).max(first) - first).min(count - 2);
        let node = ((first + index) * spacing) as f32;
        (index, (at - node) / spacing as f32)
    }

    /// The boundary coordinate at the output pixel-edge coordinate `(x, y)`, a pixel centre being
    /// `(x + ½, y + ½)`: the bilinear blend of the four nodes around it, in `f32`, as the surface
    /// evaluates it. Linear filtering by a texture sampler is not this: its weights carry too few
    /// bits to hold the tolerance.
    pub fn sample(&self, x: f32, y: f32) -> [f32; 2] {
        let spacing = self.spacing.max(1);
        let (column, fx) = Self::cell(x, spacing, self.origin.0 / spacing, self.columns);
        let (row, fy) = Self::cell(y, spacing, self.origin.1 / spacing, self.rows);
        let lerp =
            |a: [f32; 2], b: [f32; 2], t: f32| [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t];
        let top = lerp(self.node(column, row), self.node(column + 1, row), fx);
        let bottom = lerp(
            self.node(column, row + 1),
            self.node(column + 1, row + 1),
            fx,
        );
        lerp(top, bottom, fy)
    }

    /// The bytes the nodes take.
    pub fn bytes(&self) -> usize {
        self.nodes.len() * std::mem::size_of::<[f32; 2]>()
    }

    /// The largest displacement, in output pixels, at every cell centre and edge midpoint inside
    /// the region: the interpolation error in boundary texels through the inverse of the cell's
    /// own Jacobian, taken from its node differences.
    fn estimate(&self, map: &GeometryMap, region: Region) -> f64 {
        let spacing = f64::from(self.spacing);
        let (x_low, x_high) = (f64::from(region.x0) + 0.5, f64::from(region.x1()) - 0.5);
        let (y_low, y_high) = (f64::from(region.y0) + 0.5, f64::from(region.y1()) - 0.5);
        let mut worst = 0.0_f64;
        for half_row in 0..2 * (self.rows - 1) + 1 {
            for half_column in 0..2 * (self.columns - 1) + 1 {
                if half_row % 2 == 0 && half_column % 2 == 0 {
                    continue;
                }
                let x = f64::from(self.origin.0) + f64::from(half_column) * spacing / 2.0;
                let y = f64::from(self.origin.1) + f64::from(half_row) * spacing / 2.0;
                if x < x_low || x > x_high || y < y_low || y > y_high {
                    continue;
                }
                let column = (half_column / 2).min(self.columns - 2);
                let row = (half_row / 2).min(self.rows - 2);
                let origin = self.node(column, row);
                let across = self.node(column + 1, row);
                let down = self.node(column, row + 1);
                // d(u, v)/dx and d(u, v)/dy, the columns of J.
                let j = [
                    f64::from(across[0] - origin[0]) / spacing,
                    f64::from(down[0] - origin[0]) / spacing,
                    f64::from(across[1] - origin[1]) / spacing,
                    f64::from(down[1] - origin[1]) / spacing,
                ];
                let determinant = j[0] * j[3] - j[1] * j[2];
                let (u, v) = map.content_at(x, y);
                let sampled = self.sample(x as f32, y as f32);
                let (du, dv) = (f64::from(sampled[0]) - u, f64::from(sampled[1]) - v);
                let dx = (j[3] * du - j[1] * dv) / determinant;
                let dy = (j[0] * dv - j[2] * du) / determinant;
                let error = dx.hypot(dy);
                if !error.is_finite() {
                    return f64::INFINITY;
                }
                worst = worst.max(error);
            }
        }
        worst
    }
}
