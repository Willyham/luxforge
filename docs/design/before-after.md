# Before and after comparison

## Behavior and scope

Tap `\` to toggle a persistent comparison slider. Releasing within 200 ms counts as a tap; holding for 200 ms replaces the photograph with its Original until release. Shift+`\` keeps the immediate whole-original hold. Before occupies the left side, After the right; drag the vertical divider across the photograph, including to either edge. Tap `\` again or press Escape to exit. Holding `\` during slider comparison temporarily shows Before across the whole photograph and release restores the slider.

Both slider sides use the displayed entry's orientation, straighten and crop. After is the immutable entry displayed when comparison starts, including a historical selection. Exiting restores that selection. Comparison changes no recipe, history or source bytes. An open draft or in-flight edit refuses comparison with the existing reason. Text fields retain backslash input; repeated key presses never toggle the mode repeatedly. Escape cancels a pending tap/hold decision and exits comparison. Losing window focus cancels a pending decision or releases a temporary hold while keeping an existing slider open. Late deadlines and releases after cancellation do nothing.

The title bar Compare button toggles the slider, and the palette exposes the same operation. Keyboard holding remains available. Comparison suppresses editing canvas gestures, pointer sampling and clipping/mask overlays so they cannot imply which half they describe. Zoom and pan use the same image geometry on both sides. A retained full-detail After frame is preferred; if only its display proxy exists, that side remains a display preview at percentage zoom.

## Command and resource contract

`preview.compare` owns the slider selection and its normalized divider position in the client's preview session. Enabling saves the prior selection and fixes the After entry; Before selects the Original with that After entry's geometry. Disabling restores the prior selection. Position-only updates change no preview generation and request no rendering, source work or history reads. Ordinary selection changes clear comparison. The desktop calls this command through the shared registry.

Held comparison uses `preview.select` and `preview.return-current`. Entering and releasing adopt the session in the input's own update, before preparing a preview on the worker. The released selection's generation rejects a late Before answer, including when release happens before its preparation finishes.

The desktop retains at most one immutable After frame by sharing its existing RGBA allocation. The two photo surfaces use the existing aggregate GPU limits. Draw identities are recorded per active surface and removed when its textures are released, so evidence waits for both halves and reports their actual pixels. Divider motion changes clipping and chrome only: it allocates no image buffers, renders no recipe and uploads no unchanged photograph. Source access and preview preparation keep the existing workers and verified cache. One gated 200 ms keyboard deadline exists only while a press is undecided; it is removed on tap, hold recognition, Escape or focus loss. No idle timer, dependency or polling is added.

## Acceptance

- Keyboard press, release, repeats, text focus, Escape and focus loss follow the behavior above.
- Slider endpoints reproduce each side; intermediate frames preserve alignment through crop/orientation and zoom.
- Draft refusal, historical restoration, rapid exit and failed preparation keep the comparison state coherent.
- API discovery and validation describe entering, moving and exiting; independent client state remains isolated.
- Focused tests, the final quick tier and native background rendered evidence correlate divider state, pixels and logs. Performance claims require separate photo-sized measurements; none is implied by the implementation.

## Performance review

Original access remains through the verified source worker/cache; comparison's owner command reads one bounded Original history row only on entry. After retains one existing raster allocation by Arc, bounded by the existing 512 MiB byte-raster cap, and its GPU surface uses the shared 1 GiB photo ceiling. Divider moves allocate no frame, perform no point query, validation render or decoding, and read no history or asset state. Enter/exit each plans one ordinary preview, with recipe/mask rows following the existing selection completion. Position changes call only `preview.compare` and update the clipping rectangle, with unchanged texture versions. The existing keyboard subscription carries one gated 200 ms deadline only while a press is undecided; it ends on recognition, release or cancellation. It reads no photo or catalog data, and a sequence rejects late deadlines. There is no added idle work, polling or dependency; focus loss uses the existing window subscription. Geometry/clipping tests and native fixture captures check placement and pixels; sharing tests check retained allocation ownership. Photo-sized editor-performance timings have not been run and no speed or total-memory claim is made.
