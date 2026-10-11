# Corrections numerics: Clone, Heal and the automatic source

Status: **frozen by the numerical study, 2026-10-10; implemented and accepted.** The CPU reference units, the independent `f64` reference and the GPU program exist, and the render integration compiles the repair layer into these units and the GPU program described here. The corpus review below was run on the M4 and the owner accepted Heal as delivered after a blinded review on 2026-10-11. The owner set aside the quiet-host timing distributions this page waits for on 2026-10-11; they have not been run. The [Corrections contract](corrections.md) owns the operation shapes, actions, drafts, catalog, limits and rendering seams; the [Corrections workspace](corrections-workspace.md) owns what the person sees. This page owns the numbers: every equation, its order, the solver, the search and their bounds, and what was measured.

Numerical correctness and naturalness are separate evidence. The equations below are held, bit for bit, between the core's units and an independently written reference, and the GPU is held to the units within the spatial class's tolerance. Whether a repair looks natural is a photographic judgement, recorded under [visual review](#visual-review-and-failures) and reviewed on the corpus under [the corpus review](#the-corpus-review), including the owner's blinded review. A numerically exact Heal can still look wrong: it is a membrane, not an inpainting model.

## Representation

The repair stage receives the developed source in upright content coordinates, before Detail and colour ([placement](corrections.md)). Every unit operates on the spatial primitive's representation: planar `f32` linear-sRGB RGB over a rectangle of the content stage.

- **JPEG.** Each value is a decoded 8-bit code. The repair layer is a stage boundary, so the host quantizes its output to codes, as it quantizes every spatial operation's; the units themselves never clamp.
- **RAW.** The continuous development: values above 1 and slightly below 0 are kept, read and written unclamped.
- **Between operations** each operation's output is rounded to `f32`, as the frame holds it, before the next operation reads it.

Clone blends in linear light. Heal solves its correction in the **sRGB-encoded domain**: `E` and `D` are the sRGB encode and decode extended over the whole real line (`colour::srgb::encode` and `decode`, `luxforge_reference::srgb::encode_extended` and `decode_encoded`: the linear segment continues below zero, the power law above one). The automatic search compares encoded values. The [study](#why-the-encoded-domain) shows why.

## Coordinates and terms

An operation is evaluated against the content stage it renders at, `W x H` pixels (full resolution, or a reduced stage for a drag at Fit). Stored geometry is resolution independent:

| Term | Stored | At a stage |
| --- | --- | --- |
| Path positions | Normalized `[x, y]` on the grid, `x` a fraction of the content width and `y` of its height (`path::capture_grid`, 16,384 steps a unit) | Mask space `(u, v) = (x W/H, y)`: one unit is the content height on both axes, as the mask brush's |
| Radius `R` | Content heights | `R` in mask space; `R H` pixels |
| Feather `f` | Integer `0..=100` | Ramp width `band = R (f / 100)`; `band = 0` is a hard edge |
| Opacity `o` | Integer `0..=100` | `amount = o / 100` |
| Offset | Integer grid steps `[kx, ky]`, source anchor minus destination anchor | `delta = (round(kx / 16384 * W), round(ky / 16384 * H))` whole pixels |

The contract's text describes positions as "content height on both axes"; the stored positions are fractions of each axis, as `mask::brush`, `mask_draft` and the reference's `Stage::position_uv` read them, and only distances (the radius) are in content heights.

## Coverage

Coverage is the mask brush's capsule profile at the distance to the operation's nearest segment, at the pixel's centre, times the opacity:

```text
u = (px + 0.5) / H,  v = (py + 0.5) / H
segments: one per consecutive pair of path positions; a spot is one degenerate segment (e = 0, len2 = 1)
d2(segment) = |(u, v) - (a + clamp(((u, v) - a) . e / len2, 0, 1) e)|^2
d = sqrt(min over segments of d2)
profile = band == 0 ? (d <= R ? 1 : 0) : smooth(clamp((R - d) / band, 0, 1)),  smooth(s) = s^2 (3 - 2s)
c = profile * amount
```

One operation is one pass of the brush: a path that crosses itself has one density, not a build-up (the minimum distance, as the brush's "maximum along a stroke"). Coverage is the union of the path's capsules, so a stroke brushed back over itself covers, repairs and heals exactly as one pass over the same area does, bit for bit (`a_stroke_brushed_back_and_forth_is_one_pass`). The stored path keeps every turn: decimation measures a position's distance to the kept segment, not to the line through it ([masking](masking.md)), so a path painted back and forth along one row is stored with its turns and reaches where the pointer went. Coverage is exactly 1 on the core, exactly 0 at and beyond `R` on the feathered branch, and point-sampled at pixel centres, so a hard edge aliases exactly as the hard brush's does. `len2` and `band` stay divisors and the square root is taken once, so the core's `repair::footprint` is bit-identical to the reference.

## Source, interpolation and the frame's edge

- **No interpolation.** The offset becomes a whole stage pixel, rounded half away from zero; a grid step is a power-of-two fraction, so the product is exact and every implementation rounds the same number. A clone or heal source is never resampled, so it keeps its grain and sharpness; bilinear sampling at a half-pixel offset would soften fine texture and noise, which reads as a smeared patch beside unsmoothed surroundings. The registration error is at most half a pixel of the stage. At a reduced stage the rounded offset may differ from the scaled full-resolution one by half a reduced pixel.
- **Validity.** Pixel `p` is valid when `p + delta` is inside the stage. The repaired coverage is `a(p) = c(p)` where valid and `0` elsewhere: a destination pixel whose source leaves the frame is **not repaired**, keeping its input exactly. The units never clamp, mirror or wrap a source pixel. The commands store every source with its whole footprint a stage pixel inside the content frame ([contract](corrections.md)), so at full resolution this rule meets only a destination that hangs past the frame itself; it remains the units' rule for whatever they are given, and for a reduced stage's rounding.
- **The destination's edge.** A footprint clipped by the frame is simply clipped; Heal treats the frame's edge as having no boundary condition (below).

## Clone

For every pixel with `a(p) > 0`:

```text
out(p) = (1 - a) I(p) + a I(p + delta)          -- f64, then rounded to f32
```

and every other pixel is its input exactly. `(1 - a) i + a s` is exact at both ends: `a = 1` gives the source exactly. The CPU unit works in place with no scratch: rows are visited away from the source (bottom up when the source is above, top down when below, and the columns likewise when it is on the same row), so a source pixel is never one this operation has already written.

## Heal

Heal writes, for every pixel of its footprint `Ω = {p : a(p) > 0}`:

```text
out(p) = (1 - a) I(p) + a D(E(S(p)) + f(p)),     S(p) = I(p + delta)
```

where `f` is the **membrane**: zero Laplacian over `Ω`, equal on the one-pixel ring around `Ω` to the encoded boundary difference `E(I(q)) - E(S(q))`, and free (Neumann) wherever the ring leaves the stage or its source is invalid. It is the boundary-value problem darktable's and GIMP's heal solve; it adapts the source's low-frequency tone to the destination's surroundings and keeps the source's detail.

### The problem

- **The solve rectangle** is a function of geometry alone, so every implementation anchors its levels identically without reading a pixel. With `(u0, v0, u1, v1)` the segments' box: `x0 = max(0, floor((u0 - R) H - 0.5) - 1)`, `x1 = min(W, floor((u1 + R) H - 0.5) + 3)`, and likewise `y0`, `y1`. It holds every pixel within `R` of the path and its ring.
- **Classes** of the rectangle's cells (level 0): **unknown** for a pixel of `Ω`; **fixed** for a pixel outside `Ω` with a valid source that is 4-adjacent to one inside, holding `d = E(I(q)) - E(S(q))` per channel; **outside** for everything else, left out of every neighbour sum.
- **Levels.** Level `k + 1` is level `k` halved (`ceil`), each cell over its children `(2i, 2j), (2i+1, 2j), (2i, 2j+1), (2i+1, 2j+1)`: fixed if any child is fixed, else unknown if any child is, else outside. The levels go to `1 x 1`; a level past the first with no unknown changes nothing and may be left out.

### The frozen solver

1. **Start.** Up the levels, each coarse fixed cell takes the mean of its fixed children's values (summed in child order, divided by their count). Down from the coarsest, whose unknowns hold zero, each unknown takes its parent's value. So each unknown starts from the boundary mean of the nearest enclosing block that holds boundary cells.
2. **Three V(2, 2) cycles** on `a(f) = 0`, with `a(f)(c) = count f(c) - sum of non-outside neighbours`:
   - two red-black Gauss-Seidel iterations: every unknown with `(i + j)` even, then every one odd, set to `(rhs(c) + f(n1) + f(n2) + ...) / count` over its non-outside neighbours in the order `-x, +x, -y, +y`, summed from `rhs(c)`; a cell with no such neighbour keeps its value;
   - the residual `rhs - a(f)` of each unknown, restricted to the next level by summing each coarse unknown's unknown children in child order (zero elsewhere), the coarse values set to zero and one V-cycle run there;
   - each unknown whose parent is unknown corrected by its parent's value (piecewise constant);
   - two more iterations.
3. **Composite** as above, in `f64`, rounded to `f32`.

Within one colour no cell reads another of its colour, so a half iteration's order does not matter and the GPU, which runs each half as one pass, evaluates the same sums. The core's `repair::heal` holds each level in 16 x 16 tiles allocated only where a cell is not outside, so a hair-thin stroke across the frame costs its own area, not its bounding box's.

### Bounds

- **Iterations.** Fixed: the start, then `3 x (2 + 2)` iterations and a residual per level: about 20 sweeps of the level-0 cells in all, the coarser levels adding a third.
- **Scratch.** About 41 bytes per level-0 cell (`f64` values and a class byte, a right-hand side on coarse levels), reserved before allocating. One Heal may hold at most **8 Mi level-0 cells** (`repair::HEAL_CELLS_MAX`, about 350 MB while it solves) and is refused past it with `resource-limit` naming the operation. That admits a spot of radius `0.25` content heights (the contract's maximum) on a 60 MP 3:2 photograph (about 8.0 million cells with tile rounding); it refuses that radius only on photographs above about 62 MP or squarer than 3:2 at 64 MP. The contract keeps its radius range and the bound refuses by name; nothing is solved at a reduced resolution to fit.
- **Clone** needs no scratch.

### Why this solver

The study (`cargo test -p luxforge-reference --test studies repair:: -- --nocapture`) solves each case's problem with every candidate and compares the correction with the **converged membrane** (over-relaxed Gauss-Seidel to a `1e-12` update): largest and RMS difference in 8-bit encoded codes.

| Case | Unknowns | Frozen: start, 3 x V(2,2) | Cascade, 4 sweeps a level | Convolution pyramid |
| --- | --- | --- | --- | --- |
| Small spot, texture | 208 | 0.01 / 0.004 | 5.84 / 1.96 | 19.8 / 3.18 |
| Spot r = 32 px, texture | 3,228 | 0.11 / 0.042 | 6.60 / 1.48 | 15.2 / 1.39 |
| Spot r = 96 px, ramp | 28,968 | 0.02 / 0.005 | 0.31 / 0.105 | 3.42 / 1.31 |
| Spot r = 96 px, texture | 28,968 | 0.28 / 0.079 | 7.70 / 1.32 | 14.7 / 1.03 |
| Spot at the frame's edge | 1,706 | 0.76 / 0.392 | 11.2 / 4.70 | 13.3 / 1.78 |
| Thin stroke r = 6 px, texture | 4,170 | 0.30 / 0.043 | 5.40 / 1.25 | 17.3 / 2.13 |
| Stroke r = 20 px, ramp | 14,755 | 0.00 / 0.001 | 0.09 / 0.019 | 1.86 / 0.68 |
| Stroke r = 20 px across a hard edge | 14,755 | 0.47 / 0.101 | 22.6 / 2.57 | 81.4 / 16.0 |

Worst over the cases for the other schedules measured: start and 2 x V(2,2) 2.29; start and 3 x V(1,1) 6.31; start and 4 x V(1,1) 2.54; start and 2 x V(3,3) 1.23; zero start and 3 x V(2,2) 3.30; zero start and 4 x V(2,2) 1.18 codes. The frozen schedule's worst is **0.76 codes** (the spot at the frame's edge, whose Neumann side converges slowest), held below one code by the study.

- A **cascade** (coarse-to-fine with sweeps, no residual) is cheaper per level but moves and averages the boundary at each coarse level and never corrects it: on textured boundaries it is 5 to 22 codes from the membrane, as low-frequency blotches near the edge.
- A **normalized-convolution pyramid** (push-pull, no iteration) interpolates the boundary rather than solving for it: 2 to 81 codes off, and it does not meet the boundary exactly at a hard edge.
- **Over-relaxed Gauss-Seidel to convergence** is the yardstick, not a candidate: its iteration count grows with the footprint's size and has no fixed bound.
- **Mean-value coordinates** need the boundary as an ordered polygon, which a raster ring with holes, frame edges and invalid sources does not give; not pursued.

On the owner's Sapa drone photograph at full resolution (a 1,186 x 594 crop, a 13-position stroke of radius 40 px along the road, 103,924 unknowns) the frozen correction is 5.45 codes from the membrane at its largest, at one point where the source's bright patch meets the ring, and 0.57 RMS; on a spot of radius 40 px over the field's blotches, 0.05 and 0.013.

### Why the encoded domain

A source whose relative texture contrast is 7.9%, moved from a dark area (0.03 linear) into a bright one (0.5): healed additively in linear light the patch keeps the texture's absolute amplitude, so only 8% of its relative contrast survives (the patch looks flat), and a bright source healed into a dark area pushes its texture below zero. In the encoded domain 31% survives. On the photograph, the linear-domain heal of the road leaves visible dark and bright blotches where the encoded heal blends (`photographic_review`). A logarithmic domain would keep relative contrast exactly but fails at zero and the continuous range's negative values; the extended encode handles both and is the transfer function the editor already has.

## Order and overlap

Operations evaluate in stored order. Operation `i` reads only `P_i`, the layer's input with operations `0..i` applied, for its source, its destination and its ring, and writes `P_{i+1}`; nothing reads its own output or a later operation's. A stroke painted over an earlier repair repairs the repair; a source over an earlier destination samples the earlier output; reordering re-renders and moves no source. Each operation's output is rounded to `f32` before the next reads it.

A **group** is a run of consecutive operations none of whose reads (destination box, ring or source box) meets an earlier destination of the group: they all read the same input, so the GPU evaluates a group in one set of dispatches and chains groups in order. Independent dust spots are one group.

## Declared reads

Operation `i` reads two rectangles of the stage, `RepairOp::reads`: its destination (the coverage support, the path box grown by `R` and a pixel) and its source (the destination moved by `delta`), each grown by one pixel for Heal's ring and clipped to the stage. A source can be farther than any halo, so the reads are declared rectangles, never a grown halo. Producing a window of the layer's output reads, walking operations from last to first, every source and (for Heal) every whole destination whose destination meets the growing set ([contract](corrections.md)). `repair::apply` refuses a frame that does not hold an operation's declared reads before it writes anything.

## Reduced stages

Every term is resolution independent except the rounding: at a reduced stage (a drag at Fit) the offset rounds to that stage's pixel, coverage is point-sampled at its pixel centres and Heal solves on its pixels. The result approximates the full-resolution one; it is a motion difference, reported against the picture at rest and the reference, and never what is drawn at rest or exported.

## Automatic sources

The workspace's [automatic source](corrections-workspace.md#automatic-sources), frozen. All lengths in mask space unless named.

- **Extent** `E` = half the diagonal of the path positions' box plus `R` (a spot's is `R`).
- **Reduction** `r` = the least power of two with `2 E H / r <= 64`, at least 1 (never above full resolution). The prefix is reduced by `r`: each reduced pixel the mean of its `r x r` block of stage pixels (the blocks anchored at the stage's origin, edge blocks cut), summed row by row in `f64`, then encoded per channel.
- **Destination sets** at reduced pixel centres `((x + 0.5) r / H, (y + 0.5) r / H)`, distances in reduced pixels, `R_r = R H / r`, ring width `rho = max(0.25 R_r, 2)`: the footprint `F` (`d <= R_r`), the footprint grown by a reduced pixel `F+` (`d <= R_r + 1`) and the ring `Q` (`R_r + 1 < d <= R_r + 1 + rho`); `Q'` is the ring inside the reduced stage.
- **Candidates.** Rings `m in {1.5, 2.25, 3.25, 4.5}`, directions `j in 0..24` at 15° steps from `+x` (right) towards `+y` (down), their unit vectors from square roots alone (`cos 15° = (√6 + √2)/4`, ...) so the table is identical on every platform: offset `o = m E (cos, sin)`, reduced delta `round(o H / r)` per axis.
- **Rejection.** A candidate is dropped when the bounding box of `F+ ∪ Q'` moved by its delta leaves the reduced stage (its source, feather and ring would leave the content frame), or when `F+` moved by its delta meets `F+` (it overlaps the destination). A spot's 1.5 E ring always overlaps its own footprint (centres 1.5 R apart, discs of radius R), so for a spot that ring never survives; it serves strokes moved off their own axis.
- **Score**, all in encoded values:

  ```text
  A = sqrt(sum over q in Q', channels c of (J_c(q) - J_c(q + delta))^2 / (3 |Q'|))     -- 0 for empty Q'
  energy(S) = RMS over horizontally and vertically adjacent pairs within S of the per-channel difference
  T = |energy(F moved by delta) - energy(Q')|
  score = A + 1.0 T + 0.002 m
  ```

  `A` matches the candidate's surroundings to the destination's (tone and any line or edge running through); `T` matches the candidate's interior texture to the destination's surroundings, which is what keeps a source in grass rather than in the flat road beside it; the distance penalty prefers the nearer ring when the rest ties. Weight 1 for `T` puts the two RMS terms on one scale: with 0.5 a candidate straddling a flat field beat textured ones, and with 0 a flat source won outright (`search_texture_term_keeps_texture_with_texture`).
- **Ranking.** Ascending score, then nearer ring, then smaller direction; rank 1 is what an add stores. `no-source` when no candidate survives the rejection.
- **Reads.** The search is split: `search::plan` reads no pixel, applies the rejection rules and answers `no-source` before any read, and names the block-aligned region its survivors read; `search::rank` scores them over that region reduced. A long stroke's extent puts every candidate off the frame, and it answers `no-source` without reading anything.

Known answers (`luxforge-reference --test studies repair::search_`, and the core held to the reference's ranking bit for bit on each): a flat field takes ring 2.25 E at 0°; a vertical gradient ties 0° and 180° on ring and texture and takes 0°; a horizontal stroke takes ring 1.5 E at 30° (at 15° the footprints still touch); a spot by the left edge has no leftward candidate; a spot of radius 30 px in a 100 x 100 frame has none at all; a spot in a band of texture keeps its source inside the band. Every candidate's footprint lies inside the stage: a spot or stroke in any corner of a 480 × 320 to 6000 × 4000 stage, flush with the corner or hanging past it, takes only candidates whose full-resolution source rectangle is inside the stage (`automatic_sources_in_the_corners_stay_inside_the_frame`).

The search is deterministic for one input buffer. The GPU tile service may draw the reduced window directly; its area average is not bit for bit the CPU's, so a near-tie can rank differently on two hosts. That is harmless: the source is resolved once and stored as an offset.

## GPU form

`crates/luxforge-core/src/repair/repair.wgsl` (entry `lf_repair`, under the spatial convention, registered and validated with the shipped programs) and `repair/gpu.rs`, which describes one group from geometry alone in `O(operations + segments x nearby tiles + tiles)` on the planning thread. The [render integration](corrections.md#rendering) carries it through `GpuPlan`, the window walk, sweeps and incremental changes.

- **Atlas.** Every level has planes of 8 x 8 texel tiles, 256 tiles across (2,048 texels). An operation's level-0 grid is its solve rectangle cut into 8 x 8 tiles; a tile is placed when its centre lies within `R` plus its half diagonal and two pixels of the path, and a coarser tile where a child tile is. So one dispatch per pass serves every operation of a group, of any shape, and a thin stroke's planes are its area. Clone needs level 0 only. A Heal's levels stop after `floor(log2(2 R_px + 1)) + 2` (a coarse unknown needs a whole block with no fixed cell), unless its ring is broken by the frame's edge or an invalid source, when it keeps every level.
- **Block** (`lf_blocks` from the step's base): a header (operation count; offsets of the records, segments and the apply's cell index; atlas tiles across; the index's cell side, columns and rows; the stage's width and height; each level's reverse table), then one 75-word record per operation (kind, delta, radius, band, amount, segment range, solve-rectangle origin, levels, and per level its grid size, tiles across and tile-map offset), the segments (five words each, the brush's layout), the tile maps (grid tile to atlas tile), the reverse tables (atlas tile to operation and grid tile, `op << 24 | gy << 12 | gx`) and the apply's index (operations whose solve rectangle meets each 64-pixel cell of the stage).
- **Planes**, all `rgba32float`, rgb a value and alpha the class (level 0: coverage for an unknown, -1 fixed, 0 outside; coarser: 1, -1, 0): the gather (level 0's classes and boundary differences, which is also its means), per coarser level its means and right-hand side, per level a ping-pong pair of values, and the composite (level 0's repaired value and coverage), which the apply reads. Each level's planes are sized by its tile capacity, which a gesture fixes at its largest so every tick holds the same planes.
- **Passes**, each a `Texels [1, 1]` pass over its level's plane: `lf_repair_gather` (reads the input), `L - 1` `lf_repair_coarse`, `L - 1` `lf_repair_start`, then per cycle and level `lf_repair_relax` (four half iterations), `lf_repair_restrict`, the level below, `lf_repair_correct`, four more half iterations; then `lf_repair_composite` (reads the input). At most three inputs per pass (the convention allows four); one apply plane, `lf_repair_apply`, which blends `(1 - a) rgb + a value` where an operation covers the pixel. A Heal of `L` levels runs `2L + 30 (L - 2) + 24` passes: 156 at `L = 6`, 220 at 8, 284 at 10.
- **Extensions the integration needed:** fixed-size planes per level already exist (`PlaneSize::Fixed`); the group's declared reads must reach the boundary (the window walk's rectangle set, not a halo); groups chain as consecutive spatial steps of one layer, or one step whose units are groups; a group's tick changes the drafted operation's destination (a Heal's whole destination, a Clone's new segments) and the destinations of later operations whose reads meet it; a `Workgroups { count }` pass shape (one workgroup per operation) would let the coarse levels below 16 x 16 run their whole cycle in shared memory and cut the pass count by about a third.
- **Agreement.** On synthetic frames (flat, ramp, texture, hard edge and a RAW's continuous range, `rgba32float` and half-float boundaries), twelve cases of spots, strokes (one brushed back and forth over itself), near, far, overlapping and out-of-frame sources, the frame's edge and a group of three, and two chained groups, the GPU's frame is within `7.2e-6` of the CPU units' relative to the larger of one and the value (`3.8e-6` but for the back-and-forth Heal on the continuous range); at most one 8-bit code moves anywhere (mean ΔE00 0.0000, worst block 0.0022, p99 0, largest 0.57), far inside the spatial class (`cargo test -p luxforge-app --bin luxforge gpu_repair -- --nocapture`, Apple M4 Pro).

## Costs

Measured on the M4 Pro on a host shared with other agents' builds (one-minute loads up to 126 during these runs, so every figure is an upper bound and noisy). Release builds. The quiet-host timing distributions the contract asks for were set aside by the owner on 2026-10-11 and have not been run.

**GPU tick** (`gpu_repair_tick_cost`): a Heal stroke painted over 24 ticks, each adding a segment, every pass run whole each tick, through the qualifier's throughput timing (later ticks submitted without waiting). "Frame" is the same boundary with nothing to repair; "Detail" is Detail (luminance 40, colour 40, sharpening 50) evaluated incrementally where each tick's destination changed, the work the accepted repair-before-Detail placement adds downstream of a repair tick.

| Boundary | Stroke | Passes, levels | Planes | Tick | Frame | Detail over the change |
| --- | --- | --- | --- | --- | --- | --- |
| 24 MP whole (6000 x 4000) | r = 40 px | 220, 8 | 19 MiB | 6.4 to 8.5 ms | 4.5 to 7.3 ms | 0.15 to 1.0 ms (0.7% of the boundary; 5.0 to 6.4 ms whole) |
| 24 MP whole | r = 150 px | 284, 10 | 76 MiB | 10.2 to 11.7 ms | 4.6 to 6.1 ms | 0.28 to 3.3 ms (2.8%) |
| 24 MP whole | a spot of r = 0.25 H (1,000 px) | 348, 12 | 270 MiB | 34.9 to 38.2 ms | 5.0 to 6.1 ms | 1.1 to 1.2 ms (16.7%) |
| 60 MP, 4096 x 2560 at 100% | r = 60 px | 220, 8 | 29 MiB | 4.7 to 7.7 ms | 2.0 to 3.8 ms | 0.20 to 0.62 ms (2.4%) |
| 24 MP at Fit (1716 x 1144) | r = 11 px | 156, 6 | 7 MiB | 1.7 to 4.2 ms | 0.4 to 1.6 ms | 0.16 to 2.2 ms (0.7%) |

Each range is up to three runs; no run had a quiet host (one-minute loads of 57, 80, and 2.5 rising to 88 during the third), so the ranges' upper ends are contention and the lower ends the better estimate. The quiet-host distributions were set aside by the owner on 2026-10-11 and have not been run. A repair tick adds about 1 ms at Fit, 2 to 6 ms over a whole 24 MP boundary for a 40 to 150 px brush and about 30 ms for a quarter-height spot, whose solve covers 3.2 MP; encoding takes 0.07 to 3.5 ms (4 to 6.9 ms for the quarter-height spot's 50,000 tiles). At Fit the cost is mostly pass overhead (several µs a pass over tiny coarse levels), which the workgroup bottom above would cut. Detail's rerun over the changed destination is 0.15 to 1.1 ms at the lower ends (2 to 3 ms under the heaviest contention): the placement before Detail costs Detail's passes over the repair's change grown by Detail's reach, not a whole Detail frame, provided the repair change is reported as `Inside` the destination. A Heal tick changes its whole destination, so the change, and Detail's rerun, grows with the stroke; Presence links after Detail grow it again by their reach. A quarter-height Heal is 35 to 38 ms a tick at full resolution, past a display frame; at Fit the same spot is a 286 px Heal on a 1716 x 1144 stage.

**CPU reference units** (`repair::tests::photo_sized_cost`, single thread, the frame holding each operation's declared reads; the final code, load rising from 2.5 to 74 during the run):

| Case | 24 MP Heal | 24 MP Clone | 60 MP Heal | 60 MP Clone |
| --- | --- | --- | --- | --- |
| Spot r = 50 px | 4.2 ms | 0.09 ms | 2.0 ms | 0.09 ms |
| Spot r = 200 px | 30 ms | 1.3 ms | 65 ms | 1.3 ms |
| Spot r = 0.25 H (1,000 / 1,584 px) | 1.6 s | 50 ms | 7.8 s (3.5 s on an earlier, less loaded run) | 0.30 s |
| Stroke across the frame, r = 12 px | 0.13 s | 8 ms | 0.36 s | 91 ms |
| Stroke r = 100 px over 2,000 px | 0.24 s | 6 ms | 0.45 s | 6 ms |

The automatic search: its plan 0.01 to 3 ms; the reduction of its region 2 ms (spot r = 50 px, 0.3 MP read) to 0.06 to 0.2 s (a quarter-height spot or a broad stroke, 18 to 56 MP read on the CPU); the ranking 0.07 to 12 ms. A stroke across the frame plans to `no-source` with nothing read. The CPU solve is serial; parallelizing a half iteration across tiles remains unmeasured against `render::limits::parallel_pixels`.

## Visual review and failures

`cargo test -p luxforge-reference --release --test studies repair::photographic_review -- --ignored --nocapture` with `LUXFORGE_REPAIR_PHOTO` set to the owner's Sapa drone JPEG writes each case's input, Clone, frozen Heal, converged membrane, linear-domain Heal and the Heal's difference from the membrane under `artifacts/repair-study/`. Reviewed at full resolution:

- **Road removed with a stroke (r = 40 px, source 80 px above).** Clone leaves a visible tone seam along both edges of the stroke. The frozen Heal blends the slope's tone into the surroundings; the frozen and converged results are indistinguishable. Failures: where the source passes a bright dirt patch the Heal carries a bright smear into the road's line; near the frame's left edge, where the ring has no boundary condition, a shadow in the source darkens the patch; where the path missed the road's edge, thin bright road edges remain outside the footprint (a geometry miss, not a solver one).
- **Blotches healed with a spot (r = 40 px).** Clean; Heal and membrane differ by 0.05 codes.
- **Linear-domain Heal** of the road: dark and bright blotches where the encoded Heal blends. The reason the encoded domain is frozen.
- **Expected failures of the method**, inherent and not defects: healing across a hard edge smears the edge's tone into the patch; a source whose structure differs (a line, a corner) carries that structure in; very large heals take the boundary's tone but repeat the source's texture visibly. Naturalness on the corpus, JPEG and continuous RAW, is reviewed [below](#the-corpus-review).

### The corpus review

Single Heals with the automatic source on the owner's Sapa drone JPEG and the Z6, X100VI and Air 2S RAWs, 16 cases, each exported at full resolution by the reference renderer through `luxforge-json` (before, the frozen search's source, and the source the clearing-distance proposal below picks), reviewed at about three screen pixels per photograph pixel by the qualifying agent, and blind by the owner on 2026-10-11 (below). Evidence and the reproduction are under `artifacts/qualify/quality/` in the qualifying worktree: `sheets/` labelled, `blinded/` randomized and unlabelled with `blinded-key.json` beside it, `tools/drive.py`, `tools/proposal.py` and `tools/crops.py`.

- **Clean:** small objects in a uniform or textured field (a cow in a dark field, a swimmer in a lagoon, a pair of kayaks on dark water, a boat's hull on a glittering sea, a speck on terraces), and the four heals over areas with nothing to remove, which leave no trace.
- **Failures the method predicts:** a stroke across the middle of a road that continues past it pulls the road's tone in from the ring (bright smear and dark stripes); a lamp, a stem and a flag whose ring holds the object itself keep its tone as a haze or ghost; a heal over a person on a railing repeats the railing's structure with seams.
- **Geometry misses:** a round spot on an elongated kayak or a boat with its shadow leaves what lies outside the footprint.
- **Structure carried in by the automatic source:** a flag healed in the sky takes a source holding a bamboo pole, a lamp's spot a piece of rope, and on the quadrant fixture the `corrections` smoke's painted Heal over the green quadrant takes the `TOP RIGHT / GREEN` label ([below](#the-quadrant-fixtures-label)).

#### The quadrant fixture's label

The `corrections` smoke paints a Heal stroke of radius 0.04 content heights from `(0.62, 0.30)` to `(0.82, 0.30)` over the flat green quadrant of the 480 × 320 fixture. The frozen search explains its source exactly (`cargo test -p luxforge-reference --release --test studies repair::search_review_quadrants -- --ignored --nocapture`): the stroke's extent `E` is 60.8 px (half its 96 px diagonal plus the radius), so its nearest ring is 91 px away, and the quadrant is 240 × 160 px. Of the 96 candidates 38 survive the rejection rules and none lies wholly in the green: the best, ring 1.5 E at 300°, 46 px right and 79 px up, holds the label (5.4% of its footprint near-white) and scores `A` 0.049 + `T` 0.160 = 0.212, while every candidate free of white crosses into another quadrant and scores at least 0.44 on its ring term alone. The score ranks correctly; the candidate set offers nothing clean, because a stroke's rings scale with its length, not with how far it must move to clear itself. The `⇧`-click stroke beside it, shorter, finds a clean source (score 0.003).

**Considered, not adopted:** in each of the 24 directions, offsets at 1, 1.5, 2.25 and 3.25 times the least shift that clears the grown footprint `F+` in that direction (for a horizontal stroke, its width across, not its diagonal), scored by the same `A` and `T`. On the fixture it finds a clean source 46 px right and 28 px down (score 0.002) and the Heal leaves no trace. On the 16 photographic cases it is no better: comparable on 9 (the spots, where the two sets nearly coincide), worse on 3 (a blue blob pasted beside a wire, dark blotches in an erosion patch, a bright haze from a lamp), better decisively on none. **Owner's blinded review (2026-10-11).** The owner compared each case's two repairs, the frozen source and the proposed source, in randomized, unlabelled crops beside the untouched photograph, which the owner lists first by convention. Head to head the frozen and proposed sources split 6, 5 and 5 ties, so the proposal is not adopted and the frozen candidate set stands. The owner named three failures: a hard seam where a stroke crosses the road (sapa-road), a large repair on the Z6 person that does not blend (z6-person), and the lamp, which neither source repairs (z6-lamp); the cow (sapa-cow) looked good. These are the failure classes above, across a continuing line, a large heal repeating its source's structure and a ring that holds the object. The owner accepted Heal as delivered with these limits documented in the user guide; a quality improvement is later work.

## Implementation and tests

| Piece | Where |
| --- | --- |
| Independent `f64` reference | `crates/luxforge-reference/src/repair.rs` |
| Study: solvers, domain, known search answers, photographic review | `crates/luxforge-reference/tests/studies/repair.rs` |
| CPU units: coverage, Clone, Heal, search, `apply`, declared reads | `crates/luxforge-core/src/repair/{mod,footprint,heal,search}.rs` |
| Exact-buffer tests and the cost measurement | `crates/luxforge-core/src/repair/tests.rs` (`cargo test -p luxforge-core --lib repair::`) |
| GPU program and group description | `crates/luxforge-core/src/repair/{repair.wgsl,gpu.rs}`; qualification API `luxforge_core::qualification::repair` |
| GPU readback tests and the tick measurement | `crates/luxforge-app/src/app/gpu_repair_tests.rs` (`cargo test -p luxforge-app --bin luxforge gpu_repair -- --nocapture`) |

The core's units are bit-identical to the reference on flat, ramped, textured, high-contrast and continuous-range inputs; on spots from 1 px to most of the frame, hard and soft edges, half and full opacity, self-crossing strokes, far, out-of-frame and self-overlapping sources and the frame's edge; on stacks where later operations repair earlier ones and on reordered stacks; and on the search's ranking, rejections and scores.

## Open questions, with recorded defaults

- **Positions' units in the contract.** The contract's sentence that positions are in "content height on both axes" does not match the stored grid, whose `x` is a fraction of the width. Default: the stored grid as the brush reads it, as here.
- **Heal's largest footprint.** Default: keep the contract's radius range and refuse a Heal past 8 Mi level-0 cells with `resource-limit` at the stage it renders, rather than narrowing the radius or solving at a reduced resolution.
- **Reduced-stage behaviour.** Default: evaluate the same equations at the reduced stage and report the difference as motion; no full-resolution solve during a drag.
