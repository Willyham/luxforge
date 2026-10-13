//! Boundary Warp (`docs/design/panorama-merge.md`, "Boundary Warp and the crop"): a smooth mesh
//! warp of the stitch's ragged covered region onto its bounding rectangle, scaled by the amount.
//!
//! **The mesh** is a regular grid of vertices over the bounding rectangle, one every
//! [`MESH_SPACING_CELLS`] plan cells and at most [`MAX_MESH_QUADS`] quads a side, its outer
//! vertices on the rectangle's edges. Each vertex carries a displacement `D`, in output pixels;
//! between vertices `D` is bilinear. At amount `a` (0–100) the output point `b` of the bounding
//! rectangle shows the stitch at `b + (a/100)·D(b)`: the warp is part of the one output mapping,
//! so every frame is still resampled once. At 0 it is the identity.
//!
//! **Boundary targets** come from the interior covered cells: covered cells whose eight
//! neighbours are covered. A vertex on the top edge moves straight down to the lowest of the first
//! interior covered rows of the columns within half a mesh spacing of it, so the edge between two
//! vertices stays inside a step in the boundary; bottom, left and right edges likewise. A corner
//! moves to the nearest interior covered cell's centre (ties to the smaller row, then column). An
//! edge vertex with no covered column within reach is left free.
//!
//! **The interior** is the harmonic (membrane) interpolation of those targets: every interior or
//! free vertex is the mean of its neighbours, solved by successive over-relaxation in raster order
//! until no vertex moves more than [`CONVERGED_PX`], at most [`MAX_SWEEPS`] sweeps. This is the
//! smoothness term of He, Chang and Sun's rectangling energy, without its line-preserving term:
//! straight lines bend gently near the edges.

use crate::{Cancel, Error};
use serde::{Deserialize, Serialize};

/// Plan cells between mesh vertices.
pub const MESH_SPACING_CELLS: usize = 8;
/// The most mesh quads a side.
pub const MAX_MESH_QUADS: usize = 128;
/// The relaxation stops once no vertex moves more than this, in output pixels...
pub const CONVERGED_PX: f64 = 1e-4;
/// ...or after this many sweeps.
pub const MAX_SWEEPS: usize = 20_000;

/// The warp's mesh at amount 100. See the module documentation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WarpMesh {
    /// Vertices a row and a column.
    pub columns: u32,
    pub rows: u32,
    /// The bounding rectangle it spans, in output pixels.
    pub width: u32,
    pub height: u32,
    /// Each vertex's displacement in output pixels, row-major.
    pub displacement: Vec<[f64; 2]>,
}

impl WarpMesh {
    /// The identity over a `width × height` rectangle.
    pub fn identity(width: u32, height: u32) -> Self {
        Self {
            columns: 2,
            rows: 2,
            width,
            height,
            displacement: vec![[0.0; 2]; 4],
        }
    }

    fn spacing(&self) -> [f64; 2] {
        [
            (f64::from(self.width) - 1.0).max(0.0) / f64::from(self.columns - 1),
            (f64::from(self.height) - 1.0).max(0.0) / f64::from(self.rows - 1),
        ]
    }

    /// Vertex `(m, n)`'s position in the bounding rectangle.
    pub fn vertex(&self, m: usize, n: usize) -> [f64; 2] {
        let s = self.spacing();
        [m as f64 * s[0], n as f64 * s[1]]
    }

    /// The displacement at bounding-rectangle point `b`, bilinear within its quad (the nearest
    /// quad's outside the rectangle).
    #[inline]
    pub fn at(&self, b: [f64; 2]) -> [f64; 2] {
        let s = self.spacing();
        let (cols, rows) = (self.columns as usize, self.rows as usize);
        let locate = |v: f64, step: f64, count: usize| -> (usize, f64) {
            if step <= 0.0 {
                return (0, 0.0);
            }
            let t = v / step;
            let k = (t.floor().max(0.0) as usize).min(count - 2);
            (k, (t - k as f64).clamp(0.0, 1.0))
        };
        let (m, fx) = locate(b[0], s[0], cols);
        let (n, fy) = locate(b[1], s[1], rows);
        let d = |m: usize, n: usize| self.displacement[n * cols + m];
        let (a, bb, c, e) = (d(m, n), d(m + 1, n), d(m, n + 1), d(m + 1, n + 1));
        std::array::from_fn(|k| {
            (1.0 - fy) * ((1.0 - fx) * a[k] + fx * bb[k]) + fy * ((1.0 - fx) * c[k] + fx * e[k])
        })
    }

    /// The bounding-rectangle point that shows stitch point `p` at `amount` (0–1): the inverse of
    /// `b ↦ b + amount·D(b)`, by fixed-point iteration.
    pub fn invert(&self, p: [f64; 2], amount: f64) -> [f64; 2] {
        let mut b = p;
        for _ in 0..64 {
            let d = self.at(b);
            let next = [p[0] - amount * d[0], p[1] - amount * d[1]];
            let moved = (next[0] - b[0]).abs().max((next[1] - b[1]).abs());
            b = next;
            if moved < 1e-6 {
                break;
            }
        }
        b
    }

    /// The bytes the mesh holds.
    pub fn bytes(&self) -> usize {
        self.displacement.len() * size_of::<[f64; 2]>()
    }
}

/// Solve the mesh for the covered cells of a `gw × gh` plan grid of `cell`-pixel cells over a
/// `width × height` bounding rectangle. See the module documentation.
pub(crate) fn solve(
    covered: &[bool],
    gw: usize,
    gh: usize,
    cell: u32,
    width: u32,
    height: u32,
    cancel: &Cancel,
) -> Result<WarpMesh, Error> {
    let g = f64::from(cell);
    let centre = |k: usize| k as f64 * g + (g - 1.0) / 2.0;
    // Interior covered cells.
    let interior: Vec<bool> = (0..gw * gh)
        .map(|i| {
            let (x, y) = (i % gw, i / gw);
            (y.saturating_sub(1)..=(y + 1).min(gh - 1)).all(|yy| {
                (x.saturating_sub(1)..=(x + 1).min(gw - 1)).all(|xx| covered[yy * gw + xx])
            }) && x > 0
                && y > 0
                && x + 1 < gw
                && y + 1 < gh
        })
        .collect();
    if !interior.iter().any(|&c| c) {
        return Ok(WarpMesh::identity(width, height));
    }
    let top: Vec<Option<usize>> = (0..gw)
        .map(|x| (0..gh).find(|&y| interior[y * gw + x]))
        .collect();
    let bottom: Vec<Option<usize>> = (0..gw)
        .map(|x| (0..gh).rev().find(|&y| interior[y * gw + x]))
        .collect();
    let left: Vec<Option<usize>> = (0..gh)
        .map(|y| (0..gw).find(|&x| interior[y * gw + x]))
        .collect();
    let right: Vec<Option<usize>> = (0..gh)
        .map(|y| (0..gw).rev().find(|&x| interior[y * gw + x]))
        .collect();
    let quads = |cells: usize| {
        cells
            .saturating_sub(1)
            .div_ceil(MESH_SPACING_CELLS)
            .clamp(1, MAX_MESH_QUADS)
    };
    let (cols, rows) = (quads(gw) + 1, quads(gh) + 1);
    let mut mesh = WarpMesh {
        columns: cols as u32,
        rows: rows as u32,
        width,
        height,
        displacement: vec![[0.0; 2]; cols * rows],
    };
    let spacing = mesh.spacing();
    let reach = |step: f64| ((step / g) / 2.0).ceil() as isize;
    let cell_of = |v: f64, count: usize| -> isize {
        ((v - (g - 1.0) / 2.0) / g)
            .round()
            .clamp(0.0, count as f64 - 1.0) as isize
    };
    // Which vertices are fixed.
    let mut fixed = vec![false; cols * rows];
    let window = |centre_cell: isize, half: isize, count: usize| {
        (centre_cell - half).max(0) as usize..=((centre_cell + half) as usize).min(count - 1)
    };
    for m in 1..cols - 1 {
        let v = mesh.vertex(m, 0);
        let span = window(cell_of(v[0], gw), reach(spacing[0]), gw);
        if let Some(row) = span.clone().filter_map(|x| top[x]).max() {
            mesh.displacement[m] = [0.0, centre(row) - v[1]];
            fixed[m] = true;
        }
        let v = mesh.vertex(m, rows - 1);
        if let Some(row) = span.filter_map(|x| bottom[x]).min() {
            mesh.displacement[(rows - 1) * cols + m] = [0.0, centre(row) - v[1]];
            fixed[(rows - 1) * cols + m] = true;
        }
    }
    for n in 1..rows - 1 {
        let v = mesh.vertex(0, n);
        let span = window(cell_of(v[1], gh), reach(spacing[1]), gh);
        if let Some(column) = span.clone().filter_map(|y| left[y]).max() {
            mesh.displacement[n * cols] = [centre(column) - v[0], 0.0];
            fixed[n * cols] = true;
        }
        let v = mesh.vertex(cols - 1, n);
        if let Some(column) = span.filter_map(|y| right[y]).min() {
            mesh.displacement[n * cols + cols - 1] = [centre(column) - v[0], 0.0];
            fixed[n * cols + cols - 1] = true;
        }
    }
    for (m, n) in [(0, 0), (cols - 1, 0), (0, rows - 1), (cols - 1, rows - 1)] {
        let v = mesh.vertex(m, n);
        let mut best: Option<(f64, usize, usize)> = None;
        for y in 0..gh {
            for x in 0..gw {
                if !interior[y * gw + x] {
                    continue;
                }
                let d = (centre(x) - v[0]).powi(2) + (centre(y) - v[1]).powi(2);
                if best.is_none_or(|(b, _, _)| d < b) {
                    best = Some((d, x, y));
                }
            }
        }
        if let Some((_, x, y)) = best {
            mesh.displacement[n * cols + m] = [centre(x) - v[0], centre(y) - v[1]];
            fixed[n * cols + m] = true;
        }
    }
    // Harmonic interior by successive over-relaxation.
    let omega = 2.0 / (1.0 + (std::f64::consts::PI / cols.max(rows) as f64).sin());
    for sweep in 0..MAX_SWEEPS {
        if sweep % 128 == 0 {
            cancel.check()?;
        }
        let mut moved = 0.0_f64;
        for n in 0..rows {
            for m in 0..cols {
                let i = n * cols + m;
                if fixed[i] {
                    continue;
                }
                let mut sum = [0.0; 2];
                let mut count = 0.0;
                for (dm, dn) in [(-1_isize, 0_isize), (1, 0), (0, -1), (0, 1)] {
                    let (mm, nn) = (m as isize + dm, n as isize + dn);
                    if mm < 0 || nn < 0 || mm >= cols as isize || nn >= rows as isize {
                        continue;
                    }
                    let d = mesh.displacement[nn as usize * cols + mm as usize];
                    sum[0] += d[0];
                    sum[1] += d[1];
                    count += 1.0;
                }
                let old = mesh.displacement[i];
                let new = [
                    old[0] + omega * (sum[0] / count - old[0]),
                    old[1] + omega * (sum[1] / count - old[1]),
                ];
                moved = moved
                    .max((new[0] - old[0]).abs())
                    .max((new[1] - old[1]).abs());
                mesh.displacement[i] = new;
            }
        }
        if moved < CONVERGED_PX {
            break;
        }
    }
    Ok(mesh)
}
