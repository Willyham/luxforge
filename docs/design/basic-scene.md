# Basic on the scene scale: Exposure, Temperature and Tint

Status: implemented on the CPU reference and in WGSL (TASK-006, level C of the [Lightroom alignment](lightroom-alignment.md#basic-realigned)), measured against Lightroom Classic's exports through the CPU reference renderer on Linux. The unit's corpus cells are within their limits on the M4 Pro's GPU in the release gate's whole-corpus run, its sheets are reviewed by the owner, and round 3 confirms it between its measured values. The independent [`f64` reference](../../crates/luxforge-reference/src/scene.rs) and this document specify the unit; production is [`scene.rs`](../../crates/luxforge-core/src/modules/basic/scene.rs) with its program [`scene.wgsl`](../../crates/luxforge-core/src/modules/basic/scene.wgsl).

## Why

A JPEG's linear values are display-referred: the camera or editor that made it already rendered them through a curve that compresses the highlights towards white. Multiplying those values by `2^EV` clips the highlights hard at a raised Exposure, and a Bradford adaptation of them tints white itself at a warm or cool setting. Lightroom Classic does neither on a rendered JPEG: its exports of the round 1 grey wedge show Exposure applying the nominal stops in the deepest shadows, about 1.4 times the stops in the midtones and rolling the highlights off towards white, and its Temperature and Tint keeping white white while the cast lives in the midtones and shadows. Both behave as a gain applied after the rendering is undone and before it is redone, which is what this unit does.

## The scene scale

For a positive linear value `x`, with `u = log2(x)`:

```text
S(x) = u + h(u)
```

`h`, the lift, is piecewise linear in `u` over 33 knots: every 0.375 stop from −9 to −1.5, then every 0.125 stop to 0, linear white. Below −9 `h` is 0, so the deepest shadows take an exact gain; above 0 it holds its last value, so values past white, a RAW photo's headroom, take an exact gain too and a lower Exposure still recovers them. Between, `h` falls through the midtones and rises just below white. `S` is strictly increasing and inverts exactly segment by segment. Zero and negative values keep their linear value on the scene scale, so a shift multiplies them by its gain, continuous with the deepest shadows.

The lift is fitted to measured outputs alone ([A4](lightroom-alignment.md#decided)): Lightroom Classic 15.6's exports of the round 1 wedge at ten Exposure values from −5 to +5, by least squares on `S(out) − S(in) = EV` over every unclipped code, with a smoothness penalty and every segment's slope `dS/du` held between 0.3 and 4. The uncapped fit has a top segment of slope 7.7, which stretched the last few codes below white by as much when a later step darkened them, a banding risk; the cap of 4 costs 0.4 code of mean error (1.6 codes over the wedge, against 1.2) and darkens white slightly at negative Exposure, where Lightroom keeps it.

## The unit

Basic's Temperature, Tint and Exposure compile to one unit, the first of the run:

```text
out_c = S⁻¹(S(in_c) + stops_c)        stops_c = T_c(temperature) + N_c(tint) + exposure
```

`T_c` and `N_c` are each channel's stops, red, green and blue, frozen at the slider values −100, −75, −50, −25, −10, 0, 10, 25, 50, 75 and 100 and read by linear interpolation; Temperature's and Tint's add. They were fitted per channel to Lightroom's exports of the wedge and the patch grid at each value, by the least mean code difference, through the frozen scale. Because shifts on one scale add, white balance before Exposure is one shift, which is their frozen order exactly; a neutral payload compiles no unit.

On a RAW photo the input is already scene-referred: the development's linear planes, which the Look renders afterwards. There the unit applies the same stops as plain gains, `out_c = in_c × 2^stops_c`, as Lightroom's Exposure on a RAW file is a gain before its rendering; measured against Lightroom's exports with the Look kept, the scene shift there is about 1.4 times too strong. The registry tells the unit which: a source-stage layer (the RAW development) leading the stack makes every later layer's input scene-referred (`CompileStage::scene_referred`), and Auto tone's forward model compiles Basic the same way. The picker solves in plain stops over a RAW photo's patch. Exposure, Temperature and Tint need no [slider response](lightroom-alignment.md#basic-realigned): the measured unit is the alignment.

The CPU unit evaluates in `f64` from the `f32` pixel and narrows once, within `1e-6 + 1e-6 × |reference|` of the reference; the WGSL program evaluates in `f32`, held to the CPU within the pointwise output limits. Its four uniform words are the channels' stops and whether the input is scene-referred.

## The neutral picker

The picker averages its patch and refuses clipped, near-black and non-finite patches as the [white balance design](basic-white-balance.md#the-neutral-picker) specifies. It then solves, by Newton's method from neutral with a central-difference Jacobian, the Temperature and Tint whose stops make the patch's three scene logs equal, which renders it neutral, and rounds each to a whole step. A value past ±100 is refused as out of range, never clamped.

## Measured

Through the CPU reference renderer on the round 1 charts, the worst median CIEDE2000 at a threshold distance or more from the default, at the identity: Exposure 1.0, Temperature 4.7, Tint 2.5.

## Limitations

- At −2 EV and below the tones just below white stay brighter than Lightroom's, by up to 30 codes at −5: one monotone scale cannot follow Lightroom at both ends.
- At Temperature's extremes the red channel's shift varies with level in Lightroom's exports, which one stop per channel cannot follow (up to 33 codes of mean red error at −100, against 3 to 8 for green and blue).
- A derived source (an HDR merge) has no source-stage layer, so it takes the scene shift though its values are scene-linear.
- Below 100% a paged derived source is drawn from its overview, the stack over its reduced resident level. The scale bends, so the unit over averaged detail differs from the reduced output at edges, and the overview's pointwise worst-block limit is 2.5 for that reason ([GPU-first](gpu-first.md#the-contract)).
