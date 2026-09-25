//! The `brush` component kind: the frozen brush of `docs/design/mask-study.md#the-brush`, compiled
//! against the content stage its mask's layer receives.
//!
//! This file is the production transcription of that block and of the `brush_coverage`,
//! `stroke_coverage`, `segment_distance2` and `capsule_profile` functions of the independent `f64`
//! reference at `crates/lightwell-core/tests/reference/mask.rs`. The per-pixel expressions are
//! written in the reference's form and the reference's order, so coverage is bit-identical to it
//! rather than merely within tolerance: a stroke's `R`, `band`, `hard` and `amount` and each
//! segment's `ax, ay, ex, ey, len2` are hoisted to compile time because they do not depend on the
//! pixel, `len2` and `band` stay **divisors** rather than becoming precomputed reciprocals, the
//! square root is taken once per stroke and not once per segment, and neither fold is rewritten into
//! its algebraically equal twin.
//!
//! **What a brush component holds.** Its strokes, in order, as content addresses into the host's
//! [stroke store](crate::path). The payload carries the reserved [`crate::path::STROKES_FIELD`] and
//! nothing else: **no coordinate is ever written into a component payload**, because every history
//! entry stores a complete recipe and a payload that embedded its points would copy every earlier
//! stroke into every later entry. A reference the store cannot answer is refused by name where the
//! recipe is compiled, never resolved to an empty stroke.
//!
//! **Why maximum along a stroke and screen union across strokes.** One pass of the brush has one
//! density whatever the pointer's sampling rate: coverage is a function of the path, not of how fast
//! the hand moved or of how many positions the desktop posted. A second pass is a second object and
//! does build up — that is the difference between a stroke and a component, and it is why the
//! per-stroke rule is a maximum while the across-stroke rule is a union.
//!
//! **Why the strokes are stored in order.** Add strokes combine by screen union, which is
//! commutative, so removing one is well defined; an erase stroke does not commute with an add, and
//! the order is where that lives.
//!
//! **Why a grid index.** A point query must be answerable in bounded time and must never rasterize
//! ([performance rule 4](../../../docs/engineering/performance-rules.md)). The index over segments is
//! what makes the cost of a pixel depend on the strokes *near it* rather than on how many strokes the
//! component holds, and the occupancy cap is checked when the mask compiles — before any pixel is
//! read.
//!
//! Compiling uses the **stored position** spelling of mask space (`u = x · W/H`, `v = y`); the
//! per-pixel path receives the pixel-centre spelling from [`super::CompiledMask`]. The two agree to
//! `2.220e-16` and not bit for bit, so each is used only where the study froze it.
use super::{DISTANCE_MAX, DISTANCE_MIN, range, smooth};
use crate::{
    Component, Error, ErrorKind,
    modules::{Region, Stage},
    path::{Stroke, StrokeId, StrokeTable},
};
use serde::{Deserialize, Serialize};

/// The token a stored component of this kind carries.
pub(super) const KIND: &str = "brush";

/// Strokes one brush component may hold. A component is a thing a person names, combines, inverts
/// and subtracts with; sixty-four strokes is far past the point where a second component is the
/// better answer, and the bound is what keeps one component's compile and one pixel's worst case
/// stated rather than open. Refused with a `ResourceLimit` error naming it.
pub const STROKES_PER_COMPONENT: usize = 64;

/// Segments one pixel may be made to test, which is the grid index's cell occupancy. It is the
/// declared limit of `docs/design/masking.md#resource-and-responsiveness-constraints` and it is
/// checked **when the mask compiles, before any pixel is read**, so a mask that would make a point
/// query unbounded is refused rather than rendered slowly.
pub const SEGMENTS_PER_PIXEL: usize = 64;

/// Cells the index spans on each axis, at most. The cell side is the component's largest stroke
/// radius, which is the scale at which a segment stops mattering to a pixel; this bound is what keeps
/// a component drawn with a tiny brush across a whole frame from asking for a grid of millions of
/// cells, and it costs nothing but occupancy, which the cap above already governs.
const GRID_SIDE_MAX: usize = 64;

/// A brush component's stored payload: its strokes, in order, by content address.
///
/// One field, and it is the host's reserved one, so the host can see the references a payload
/// carries without parsing a payload that belongs to this kind. `deny_unknown_fields` is what makes
/// a payload carrying anything else a named refusal rather than a silently half-read edit.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrushStrokes {
    pub strokes: Vec<StrokeId>,
}

/// Parse and check one stored `brush` payload, without a stage and without the store.
///
/// What is checkable here is the payload's shape and the stroke count; the strokes themselves are
/// resolved when the component is compiled, because that is where the store is in hand. Each refusal
/// names the component.
pub(super) fn parse(component: &Component) -> Result<BrushStrokes, Error> {
    let brush: BrushStrokes =
        serde_json::from_value(component.payload.clone()).map_err(|error| {
            Error::new(
                ErrorKind::Validation,
                format!(
                    "component {} has an invalid {KIND} payload: {error}",
                    component.name
                ),
            )
        })?;
    if brush.strokes.len() > STROKES_PER_COMPONENT {
        return Err(Error::new(
            ErrorKind::ResourceLimit,
            format!(
                "component {} has {} strokes; the limit is {STROKES_PER_COMPONENT} strokes per \
                 brush component",
                component.name,
                brush.strokes.len()
            ),
        ));
    }
    Ok(brush)
}

/// One segment of a stroke in mask space, with the terms that do not depend on the pixel already
/// computed.
///
/// The degenerate case — a one-point stroke, or a repeated position a reparsed store could hold — is
/// normalized at compile time to `ex = ey = 0`, `len2 = 1`, which makes the projection below evaluate
/// `t = 0/1 = 0` and `q = A` exactly. That is the frozen spelling of "the distance to the point
/// itself", and it is why there is **no branch on the per-pixel path** and no division by a vanishing
/// length.
#[derive(Clone, Copy, Debug)]
struct Segment {
    ax: f64,
    ay: f64,
    ex: f64,
    ey: f64,
    len2: f64,
}

impl Segment {
    fn new(a: (f64, f64), b: (f64, f64)) -> Self {
        let ex = b.0 - a.0;
        let ey = b.1 - a.1;
        let len2 = ex * ex + ey * ey;
        if len2 == 0.0 {
            Self {
                ax: a.0,
                ay: a.1,
                ex: 0.0,
                ey: 0.0,
                len2: 1.0,
            }
        } else {
            Self {
                ax: a.0,
                ay: a.1,
                ex,
                ey,
                len2,
            }
        }
    }

    /// The **squared** distance from a mask-space point to this segment, as the study froze it:
    ///
    /// ```text
    /// wx = u - ax
    /// wy = v - ay
    /// t  = clamp((wx*ex + wy*ey) / len2, 0, 1)
    /// qx = ax + t*ex
    /// qy = ay + t*ey
    /// dx = u - qx
    /// dy = v - qy
    /// d2 = dx*dx + dy*dy
    /// ```
    ///
    /// Squared, so the capsule profile takes one square root per stroke rather than one per segment:
    /// `sqrt` is monotone and correctly rounded, so the root of the smallest square is the smallest
    /// root, bit for bit. `len2` is a divisor and never a precomputed reciprocal, and the dot product
    /// is not reassociated.
    #[inline]
    fn distance2(&self, u: f64, v: f64) -> f64 {
        let wx = u - self.ax;
        let wy = v - self.ay;
        let t = ((wx * self.ex + wy * self.ey) / self.len2).clamp(0.0, 1.0);
        let qx = self.ax + t * self.ex;
        let qy = self.ay + t * self.ey;
        let dx = u - qx;
        let dy = v - qy;
        dx * dx + dy * dy
    }

    /// The axis-aligned mask-space box of this segment, grown by `r`: the region outside which this
    /// segment's profile is exactly zero.
    fn grown_box(&self, r: f64) -> [f64; 4] {
        let bx = self.ax + self.ex;
        let by = self.ay + self.ey;
        [
            self.ax.min(bx) - r,
            self.ay.min(by) - r,
            self.ax.max(bx) + r,
            self.ay.max(by) + r,
        ]
    }
}

/// One stroke bound to a stage: its segments and the handful of per-stroke terms.
#[derive(Clone, Debug)]
struct CompiledStroke {
    /// The radius in mask-space units, kept because the profile compares `d` against it directly.
    r: f64,
    /// `R · feather/100`, the ramp's width, kept as a **divisor** rather than a reciprocal.
    band: f64,
    /// The study's explicit `feather = 0` case, exactly as the radial's `hard` is: the division by
    /// `band` lives on the other branch, so a vanishing band is never divided by. It is set from the
    /// computed band rather than from the stored feather, which absorbs the second route to the same
    /// edge — a feather so small that `R · f/100` underflows to exactly zero.
    hard: bool,
    /// `flow / 100`, the amount one pass reaches.
    amount: f64,
    erase: bool,
    segments: Vec<Segment>,
    /// The colour similarity this stroke multiplies its coverage by, when it carries one, already
    /// compiled: the seed's Oklab `(a, b)` and the radius its refine maps to, computed once here
    /// because neither depends on the pixel.
    ///
    /// It is the [colour range](super::range)'s own compiled falloff at **one sample** — the same
    /// type, the same metric, the same mapping — so there is no second colour space and no second
    /// constant anywhere in the editor because of the colour-constrained brush
    /// (`docs/design/mask-study.md#the-colour-constraint`).
    colour: Option<range::CompiledColour>,
}

impl CompiledStroke {
    /// The capsule profile at a distance, as the study froze it:
    ///
    /// ```text
    /// s = if hard { if d <= R { 1 } else { 0 } }
    ///     else    { smooth(clamp((R - d) / band, 0, 1)) }
    /// ```
    ///
    /// Exactly `1.0` on the core and exactly `0.0` at and beyond `R` on the feathered branch, because
    /// `smooth` is exact at both clamp ends. That exactness is what makes the support box and the
    /// grid index exactly right rather than nearly right.
    #[inline]
    fn profile(&self, d: f64) -> f64 {
        if self.hard {
            if d <= self.r { 1.0 } else { 0.0 }
        } else {
            smooth(((self.r - d) / self.band).clamp(0.0, 1.0))
        }
    }

    /// This stroke's coverage from the **squared** distance to its nearest reached segment, and the
    /// pixel the operation this mask modulates receives.
    ///
    /// The caller supplies that minimum, because the grid index is what decides which segments a
    /// pixel tests. A pixel that reached none of this stroke's segments does not call this at all:
    /// its coverage is exactly `0.0` and the fold is an exact identity there.
    ///
    /// ```text
    /// s = capsule_profile(d) * (flow / 100)              -- unlimited
    /// s = capsule_profile(d) * (flow / 100) * k          -- limited to a colour
    /// ```
    ///
    /// The similarity is the **last** multiply and only a limited stroke performs one: an unlimited
    /// stroke evaluates the expression it always did, with no `* 1.0` appended, so the delivered
    /// brush's bit-identity with the frozen reference is untouched. `(profile · amount) · k` is the
    /// frozen association; `profile · (amount · k)` is algebraically equal, within tolerance and not
    /// bit-identical, and the study forbids it like every other reassociation.
    #[inline]
    fn coverage_from(&self, nearest2: f64, rgb: [f64; 3]) -> f64 {
        let d = nearest2.sqrt();
        let s = self.profile(d) * self.amount;
        match &self.colour {
            None => s,
            Some(limit) => s * limit.coverage(rgb),
        }
    }
}

/// The uniform grid index over a component's segments.
///
/// Each segment is recorded in **every cell its own grown box reaches**, so a pixel tests exactly the
/// segments listed in the one cell it falls in — not a neighbourhood, and no second pass. A segment
/// absent from that cell cannot reach any point of the cell, so its stroke's profile there is exactly
/// `0.0` and the frozen fold's `s = 0` identity makes skipping it bit-identical to folding it. That
/// equality is the whole justification, and the study states it as a property of the accumulation
/// rather than of this structure.
///
/// A pixel outside the grid entirely tests nothing and composes to exactly `0.0`, which is what an
/// empty stroke list composes to as well.
#[derive(Clone, Debug)]
struct Index {
    /// The grid's minimum corner in mask space.
    u0: f64,
    v0: f64,
    /// The cell side in mask-space units, a **divisor**.
    cell: f64,
    cols: usize,
    rows: usize,
    /// `(stroke index, segment index)` per cell, in stored stroke order within each cell, so a
    /// pixel's fold visits strokes in the order the component stores them.
    cells: Vec<Vec<(u32, u32)>>,
}

impl Index {
    /// Build the index and check the occupancy cap while building it.
    ///
    /// **Memory, and the limit that bounds it** ([performance rule
    /// 6](../../../docs/engineering/performance-rules.md)). The grid is sized so its cell count
    /// never exceeds the component's own segment count — the side is `floor(sqrt(segments))`, capped
    /// at [`GRID_SIDE_MAX`] — and no cell may hold more than [`SEGMENTS_PER_PIXEL`] entries. So an
    /// index costs at most `SEGMENTS_PER_PIXEL · cells` pairs of `u32`, which is `O(segments)` and
    /// never scales with the stage: a mask's whole index set is bounded by the declared
    /// [`crate::POINTS_PER_MASK`], at 8 bytes a pair. Nothing here allocates a plane at any size.
    ///
    /// **Build cost** is bounded by the same two numbers, because the loop stops at the first cell
    /// that passes the cap rather than filling the grid and measuring afterwards. It reads no pixel,
    /// and the insertion walks a segment's box row by row rather than filling its whole bounding
    /// rectangle, so a long diagonal stroke costs the cells it reaches and not the cells it spans.
    fn build(strokes: &[CompiledStroke], mask: &str, component: &str) -> Result<Self, Error> {
        let mut extent: Option<[f64; 4]> = None;
        let mut largest = 0.0f64;
        let mut segments = 0usize;
        for stroke in strokes {
            largest = largest.max(stroke.r);
            segments += stroke.segments.len();
            for segment in &stroke.segments {
                let grown = segment.grown_box(stroke.r);
                extent = Some(match extent {
                    None => grown,
                    Some(held) => [
                        held[0].min(grown[0]),
                        held[1].min(grown[1]),
                        held[2].max(grown[2]),
                        held[3].max(grown[3]),
                    ],
                });
            }
        }
        let Some([u0, v0, u1, v1]) = extent else {
            return Ok(Self::empty());
        };
        // The cell side is the largest radius — the scale at which a segment stops mattering to a
        // pixel — widened if that would ask for more cells than the component has segments. A
        // coarser cell only raises occupancy, which the cap governs; it can never make the index
        // wrong, because a segment is recorded in every cell its own box reaches whatever the cells
        // are. Sizing the grid from the segment count is what keeps its memory `O(segments)`: one
        // cell per segment is the point past which more cells buy nothing but allocation.
        let side = segments.isqrt().clamp(1, GRID_SIDE_MAX);
        let span = (u1 - u0).max(v1 - v0);
        let cell = largest.max(span / side as f64);
        if !cell.is_finite() || cell <= 0.0 {
            return Ok(Self::empty());
        }
        let cols = cell_span(u1 - u0, cell, side);
        let rows = cell_span(v1 - v0, cell, side);
        let mut index = Self {
            u0,
            v0,
            cell,
            cols,
            rows,
            cells: vec![Vec::new(); cols * rows],
        };
        for (s, stroke) in strokes.iter().enumerate() {
            for (g, segment) in stroke.segments.iter().enumerate() {
                index.insert(s as u32, g as u32, segment, stroke.r, mask, component)?;
            }
        }
        Ok(index)
    }

    fn empty() -> Self {
        Self {
            u0: 0.0,
            v0: 0.0,
            cell: 1.0,
            cols: 0,
            rows: 0,
            cells: Vec::new(),
        }
    }

    /// Record one segment in every cell its grown box reaches, refusing as soon as a cell passes the
    /// occupancy cap.
    fn insert(
        &mut self,
        stroke: u32,
        segment_index: u32,
        segment: &Segment,
        r: f64,
        mask: &str,
        component: &str,
    ) -> Result<(), Error> {
        let [bu0, bv0, bu1, bv1] = segment.grown_box(r);
        let (col0, col1) = self.cell_range(bu0 - self.u0, bu1 - self.u0, self.cols);
        let (row0, row1) = self.cell_range(bv0 - self.v0, bv1 - self.v0, self.rows);
        for row in row0..=row1 {
            for col in col0..=col1 {
                let cell = &mut self.cells[row * self.cols + col];
                cell.push((stroke, segment_index));
                if cell.len() > SEGMENTS_PER_PIXEL {
                    return Err(Error::new(
                        ErrorKind::ResourceLimit,
                        format!(
                            "mask {mask} component {component} puts more than \
                             {SEGMENTS_PER_PIXEL} stroke segments over one pixel; the limit is \
                             {SEGMENTS_PER_PIXEL} segments tested per pixel by a brush component"
                        ),
                    ));
                }
            }
        }
        Ok(())
    }

    /// The inclusive cell range a closed interval reaches, clamped to the grid. Inclusive because a
    /// point at exactly `d = R` is covered on the hard branch, so the box's own edge must be inside
    /// the cells the segment is recorded in.
    fn cell_range(&self, low: f64, high: f64, limit: usize) -> (usize, usize) {
        let first = (low / self.cell).floor();
        let last = (high / self.cell).floor();
        let first = if first.is_finite() {
            (first as i64).clamp(0, limit as i64 - 1) as usize
        } else {
            0
        };
        let last = if last.is_finite() {
            (last as i64).clamp(0, limit as i64 - 1) as usize
        } else {
            limit - 1
        };
        (first.min(last), first.max(last))
    }

    /// The segments a pixel tests: the one cell it falls in, or nothing at all when it falls outside
    /// the grid.
    #[inline]
    fn at(&self, u: f64, v: f64) -> &[(u32, u32)] {
        if self.cells.is_empty() {
            return &[];
        }
        let col = ((u - self.u0) / self.cell).floor();
        let row = ((v - self.v0) / self.cell).floor();
        if !(col >= 0.0 && col < self.cols as f64 && row >= 0.0 && row < self.rows as f64) {
            return &[];
        }
        &self.cells[row as usize * self.cols + col as usize]
    }
}

/// Cells an extent needs at a given cell side: at least one, and one more than the quotient's floor
/// so the far edge is inside the grid, never more than `side + 1`.
fn cell_span(extent: f64, cell: f64, side: usize) -> usize {
    let cells = (extent / cell).floor();
    if !cells.is_finite() || cells < 0.0 {
        return 1;
    }
    ((cells as usize) + 1).clamp(1, side + 1)
}

/// One brush component bound to a stage: its strokes' compiled terms, the index over their segments,
/// and the conservative support box the two were built from.
#[derive(Clone, Debug)]
pub(super) struct Compiled {
    strokes: Vec<CompiledStroke>,
    index: Index,
    /// The mask-space box of the **add** strokes, outside which coverage is exactly zero, or `None`
    /// when this component covers nothing anywhere.
    support: Option<[f64; 4]>,
    /// The narrowest ramp any stroke draws, in mask-space units, or `INFINITY` when no stroke does.
    narrowest_band: f64,
}

impl Compiled {
    /// Bind a parsed payload to `stage`, resolving its strokes against the store the recipe was read
    /// from.
    ///
    /// Every refusal names what it refused: a reference the store cannot answer is the store's own
    /// `incompatible` error unchanged, a stroke whose radius is not a legal mask-space distance is
    /// `validation` naming the component and the stroke, and a component whose densest cell passes
    /// the occupancy cap is `resource-limit` naming the mask and the component. All three happen
    /// here, which is **before any pixel is read**: the host compiles a recipe before it persists
    /// one, so a brush that could not be evaluated cannot be committed either.
    pub(super) fn new(
        stored: &BrushStrokes,
        stage: Stage,
        strokes: &StrokeTable,
        mask: &str,
        component: &str,
    ) -> Result<Self, Error> {
        let aspect = f64::from(stage.width) / f64::from(stage.height);
        let mut compiled = Vec::with_capacity(stored.strokes.len());
        let mut support: Option<[f64; 4]> = None;
        let mut narrowest_band = f64::INFINITY;
        for id in &stored.strokes {
            // The refusal reads exactly as admission's does for the same reference, because it is
            // the same fact reached from the other side: admission refuses to write it, and
            // compiling refuses to draw it.
            let stroke = strokes.resolve(id).map_err(|error| {
                Error::new(
                    error.kind,
                    format!(
                        "{} referenced by component {component} of mask {mask}",
                        error.detail
                    ),
                )
            })?;
            let r = stroke.size();
            if !r.is_finite() || !(DISTANCE_MIN..=DISTANCE_MAX).contains(&r) {
                return Err(Error::new(
                    ErrorKind::Validation,
                    format!(
                        "component {component} stroke {id} size must be a number within \
                         {DISTANCE_MIN:e}..={DISTANCE_MAX:.0} mask-space units"
                    ),
                ));
            }
            let band = r * (stroke.feather() / 100.0);
            let amount = stroke.flow() / 100.0;
            let segments = segments_of(stroke, aspect);
            // The support is the union of the **add** strokes' grown boxes. An erase stroke cannot
            // raise coverage — `c · (1 - s)` only ever lowers it — and a stroke whose flow is exactly
            // zero contributes `s = profile · 0.0`, which is exactly `0.0` and an exact identity in
            // the fold, so neither widens the box.
            if !stroke.erase() && amount != 0.0 {
                for segment in &segments {
                    let grown = segment.grown_box(r);
                    support = Some(match support {
                        None => grown,
                        Some(held) => [
                            held[0].min(grown[0]),
                            held[1].min(grown[1]),
                            held[2].max(grown[2]),
                            held[3].max(grown[3]),
                        ],
                    });
                }
            }
            // Every stroke that contributes at all draws its ramp into the field, an erase stroke
            // included: what a proxy pixel has to resolve is the narrowest transition anywhere in
            // the component, not only the ones that add. A stroke whose flow is exactly zero draws
            // nothing and contributes no feature.
            if amount != 0.0 {
                narrowest_band = narrowest_band.min(band);
            }
            compiled.push(CompiledStroke {
                r,
                band,
                hard: band == 0.0,
                amount,
                erase: stroke.erase(),
                segments,
                // A stored limit is compiled here, once, for the same reason every other per-stroke
                // term is: neither the seed's Oklab pair nor the refine radius depends on the pixel.
                // The seed is read from the stroke and **never** sampled again — that is what makes a
                // stroke reproducible from its own bytes after any later edit.
                colour: stroke
                    .colour_limit()
                    .map(|limit| range::similarity(limit.seed(), limit.refine())),
            });
        }
        let index = Index::build(&compiled, mask, component)?;
        Ok(Self {
            strokes: compiled,
            index,
            support,
            narrowest_band,
        })
    }

    /// Coverage at a mask-space point, as the study froze it:
    ///
    /// ```text
    /// c = 0
    /// for stroke in strokes:                   -- stored order
    ///     d = sqrt(min over the stroke's segments of d2)
    ///     s = profile(d) * amount
    ///     c = c * (1 - s)                      if the stroke erases
    ///     c = c + (1 - c) * s                  otherwise
    /// ```
    ///
    /// Only the strokes the index says reach this pixel are folded. That is bit-identical and not
    /// merely close: a stroke absent from the pixel's cell has `s` exactly `0.0` there, and both
    /// folds are written in the one spelling that is an exact identity at `s = 0` —
    /// `c + (1 - c)·0` is `c` and `c · (1 - 0)` is `c`, bit for bit, where the algebraically equal
    /// `1 - (1 - c)(1 - s)` is not.
    ///
    /// The nearest distance is taken over the segments the cell lists, which is the same minimum the
    /// whole stroke would give: a segment the cell omits cannot reach the pixel, so it cannot be the
    /// nearest one unless every present segment is also out of reach — in which case both answers
    /// give exactly `0.0`.
    pub(super) fn coverage(&self, u: f64, v: f64, rgb: [f64; 3]) -> f64 {
        let listed = self.index.at(u, v);
        if listed.is_empty() {
            return 0.0;
        }
        let mut c = 0.0;
        let mut at = 0usize;
        while at < listed.len() {
            let stroke_index = listed[at].0;
            let stroke = &self.strokes[stroke_index as usize];
            let mut nearest2 = f64::INFINITY;
            while at < listed.len() && listed[at].0 == stroke_index {
                let d2 = stroke.segments[listed[at].1 as usize].distance2(u, v);
                if d2 < nearest2 {
                    nearest2 = d2;
                }
                at += 1;
            }
            let s = stroke.coverage_from(nearest2, rgb);
            c = if stroke.erase {
                c * (1.0 - s)
            } else {
                c + (1.0 - c) * s
            };
        }
        c
    }

    /// Whether any stroke of this component is limited to a colour, which is what makes the
    /// component's coverage a function of the pixel's value and not of its position alone.
    ///
    /// The consequences are the range selections' own, stated there and not softened here: the
    /// coverage overlay has to read the masked operation's input to draw such a mask at all, and what
    /// a limited stroke paints moves when a layer ahead of the masked one changes that input. Its **geometric** feature width is
    /// unaffected, because a colour test draws no ramp across the frame for a pixel grid to miss.
    pub(super) fn reads_pixels(&self) -> bool {
        self.strokes.iter().any(|stroke| stroke.colour.is_some())
    }

    /// A conservative pixel rectangle of this component's support.
    ///
    /// As drawn, coverage is exactly zero outside the add strokes' grown boxes, so the rectangle is
    /// that box brought to pixel indices. A colour limit cannot widen it: its similarity is in
    /// `[0, 1]`, so a limited stroke's coverage is never above the same stroke's unlimited coverage,
    /// and the box that bounded one bounds the other. Inverted, coverage is `1 - c`, which is exactly zero only
    /// where `c` is exactly `1` — the strokes' cores — and non-zero over the whole of the rest of the
    /// stage, so the whole stage is the honest conservative answer, exactly as an inverted radial and
    /// a whole-mask inversion answer it.
    pub(super) fn support(&self, stage: Stage, inverted: bool) -> Region {
        if inverted {
            return super::whole_stage(stage);
        }
        match self.support {
            None => super::empty_region(),
            Some([u0, v0, u1, v1]) => box_bounds(stage, u0, v0, u1, v1),
        }
    }

    /// The smallest feature this component draws at `stage`, in that stage's pixels: the narrowest
    /// ramp any of its add strokes contributes, one mask-space unit being the stage's height on both
    /// axes.
    ///
    /// A hard-edged stroke has no ramp at all and answers `0.0`, exactly as a hard-edged radial does:
    /// no pixel grid resolves it at any size, and the proxy path is told that rather than given a
    /// width the edge does not have. A component with no add stroke draws no feature and answers
    /// `INFINITY`, which is what an empty mask answers.
    pub(super) fn feature_px(&self, stage: Stage) -> f64 {
        self.narrowest_band * f64::from(stage.height)
    }
}

/// One stroke's segments in mask space, through the **stored position** spelling.
///
/// A stroke of `n >= 2` positions has `n - 1` segments; a one-point stroke has the single degenerate
/// segment `A → A`, normalized by [`Segment::new`], so every stroke has at least one and the
/// per-pixel path needs no empty case. A stored stroke always holds at least one position — the store
/// refuses an empty one — so this never returns an empty list.
fn segments_of(stroke: &Stroke, aspect: f64) -> Vec<Segment> {
    let uv: Vec<(f64, f64)> = stroke.points().map(|[x, y]| (x * aspect, y)).collect();
    if uv.len() == 1 {
        return vec![Segment::new(uv[0], uv[0])];
    }
    uv.windows(2)
        .map(|pair| Segment::new(pair[0], pair[1]))
        .collect()
}

/// The conservative pixel rectangle of a mask-space box, clipped to the stage.
///
/// Closed form, so a component's rectangle costs `O(1)`. It takes the same two deliberate slacks
/// [`super::half_plane_bounds`] and the radial's `ellipse_bounds` take, which is what "exactly zero
/// outside" demands of a rectangle computed in floating point: the box is widened by a few ulps of
/// the magnitudes involved, and the rectangle is then grown by one pixel on every side. A non-finite
/// value anywhere answers the whole stage, which is always a correct rectangle.
///
/// Mask space to pixel indices inverts the pixel-centre spelling: `u = (px + 0.5) / H`, so
/// `px = u · H - 0.5`.
fn box_bounds(stage: Stage, u0: f64, v0: f64, u1: f64, v1: f64) -> Region {
    if !u0.is_finite() || !v0.is_finite() || !u1.is_finite() || !v1.is_finite() {
        return super::whole_stage(stage);
    }
    let height = f64::from(stage.height);
    let slack = |low: f64, high: f64| 16.0 * f64::EPSILON * (low.abs() + high.abs());
    let su = slack(u0, u1);
    let sv = slack(v0, v1);
    let min_x = (u0 - su) * height - 0.5;
    let max_x = (u1 + su) * height - 0.5;
    let min_y = (v0 - sv) * height - 0.5;
    let max_y = (v1 + sv) * height - 0.5;
    if !min_x.is_finite() || !max_x.is_finite() || !min_y.is_finite() || !max_y.is_finite() {
        return super::whole_stage(stage);
    }
    let grow_low = |value: f64, limit: u32| -> u32 {
        let index = value.floor() as i64 - 1;
        index.clamp(0, i64::from(limit)) as u32
    };
    let grow_high = |value: f64, limit: u32| -> u32 {
        let index = value.floor() as i64 + 2;
        index.clamp(0, i64::from(limit)) as u32
    };
    let x0 = grow_low(min_x, stage.width);
    let y0 = grow_low(min_y, stage.height);
    let x1 = grow_high(max_x, stage.width);
    let y1 = grow_high(max_y, stage.height);
    if x1 <= x0 || y1 <= y0 {
        return super::empty_region();
    }
    Region {
        x0,
        y0,
        width: x1 - x0,
        height: y1 - y0,
    }
}
