# Orientation layer: repeated exact transforms collapse into one layer

Status: implemented and verified on the M4 Mac. The contract is in the [history spec](../specs/edit-history.md#exact-transforms); this document records the reasoning.

## Problem

Each exact transform appends its own layer. Four Rotate right actions leave four layers whose composition is the identity. The history entries are right, since four actions happened, but the recipe should describe the resulting orientation, not the gestures that reached it.

## Decision

Quarter-turns and reflections are one **orientation** layer holding the composed exact state, and it always sits ahead of the crop. The crop effect declares a later order within the geometry stage than the orientation, so the host places an orientation layer before the crop, and a transform composes into the orientation layer just before that position, keeping its identity. A transform made after a crop is re-expressed as the same transform ahead of it: the crop is carried through it in the same action, so the picture is the old one turned or reflected, and the crop's input stage, which is what a crop draft shows and what the crop's coordinates address, includes every transform. This is the Lightroom model: rotating a cropped photograph rotates the crop with it, and the crop tool always frames the turned photograph.

### Payload

Effect `lightwell.geometry.orientation`, format 1, geometry stage, payload `{"mirror": bool, "turns": 0..3}`. Its mapping is: mirror horizontally when `mirror` is true, then rotate clockwise by `turns` quarter-turns. Every one of the eight exact orientations has exactly one such payload; `{"mirror": false, "turns": 0}` is the neutral orientation with an identity mapping and unchanged stage.

The four actions keep their durable history identities (`rotate-left`, `rotate-right`, `mirror-horizontal`, `flip-vertical`) and their generated controls and API. Applying an action to a payload `(m, k)`:

| Action | Result |
| --- | --- |
| Rotate right | `(m, k + 1 mod 4)` |
| Rotate left | `(m, k + 3 mod 4)` |
| Mirror horizontal | `(not m, 4 − k mod 4)` |
| Flip vertical | `(not m, 2 − k mod 4)` |

These follow from `M ∘ R^k = R^(−k) ∘ M` and flip vertical being `R^2 ∘ M`. The mapping is compiled by composing the existing exact geometries of the mirror and of `turns` clockwise quarter-turns, so integer exactness is unchanged.

### Planning rule

- The position is where the host would place a new orientation layer: after the pixel, colour and spatial work, before the crop and before any finish layer.
- The layer just before that position is an orientation layer: `Update` it with the composed payload. Reaching the neutral orientation leaves a neutral layer, as crop-reset leaves a neutral crop; the plan model has no removal.
- Otherwise: `Commit` a new orientation layer holding the single action's state, which the host places at that position.
- A crop after that position is carried through the transform in the same action (`ActionPlan::Edits`): it selects the same content in the turned stage, so its output is its old output turned or reflected.
- An orientation layer after the crop, which the host never places there but a stored stack may hold, is folded by the next transform: with that transform it goes into the orientation ahead of the crop, the crop is carried through both, and the stored layer is left neutral in place, so the output is exactly the transform applied to what the stack showed and nothing is removed. Until a transform runs, such a stack renders in its stored order and a crop draft on it shows only the layers before the crop.
- A transform is never a no-op: every action changes the output.

### Carrying the crop

The rotated box turns and reflects with its input, and a reflection reverses the straightening angle, so a crop is carried by mapping its whole-pixel output rectangle through the orientation in box space. A quarter turn keeps the angle and swaps the box; a reflection negates the angle. At angle zero the box is the input stage, every edge stays on whole pixels and the carry is exact: the new render is byte-identical to the old one turned, and the inverse orientation carries the payload back. At any other angle the box extents are not whole pixels, so an edge measured from the far side of the box lands between pixels. The extents are kept and the origin moves to the nearest whole box pixel at which they are still covered, at most half a box pixel away; only a rectangle touching the rotated source on opposite sides has no such position and is fitted instead, which trims it by at most a pixel on an axis. The render is then the old one turned up to that sub-pixel resample, not byte-identical. The payload format and its output rounding are unchanged.

The previous `lightwell.geometry.transform` effect is removed. Stacks holding it are refused explicitly as an unavailable effect, never rewritten; use a new catalog. This follows the current-shapes rule.

### Consequences

- A stack holds one orientation layer, ahead of the crop, however many transforms were made before or after cropping.
- The crop's parameters (`edit.crop`, `crop-fit`'s `original` ratio, `recipe.describe` values) address the turned stage, the one the person sees.
- A crop draft left open while a transform commits is marked conflicted, as for any other commit. Reapply carries the draft through the change in orientation ahead of the crop by the same rule, so the frame keeps selecting what it did, and a locked ratio turns with a quarter turn.
- History, undo, restore, versions and lineage are unchanged: each action is still one entry with its complete resulting stack.
- The desktop and API keep showing four transform controls and the `edit.transform` action with its `transform` parameter.

## Acceptance

- Four Rotate right actions from an empty stack leave one neutral orientation layer and a byte-identical render of the source; four history entries exist and undo walks back through three, two and one turn.
- Every sequence of up to three actions renders byte-identically to the same sequence applied as separate exact transforms on the same synthetic and EXIF-mirrored fixtures.
- A transform after a crop composes into the orientation layer ahead of it and carries the crop: at angle zero every pixel is the previous render turned or reflected; through a straightened crop the frame moves by at most half a box pixel and keeps its extents unless it touches the rotated source.
- A stack with an orientation layer stored after the crop is folded by the next transform, and its render is exactly the stored one under that transform.
- A crop draft opened after a transform shows the turned photograph, and Reapply across a transform keeps the drafted frame on the same content.
- Specs, feature status and the user guide describe the orientation layer; `cargo xtask check` passes and the editor acceptance run covers the collapse.
