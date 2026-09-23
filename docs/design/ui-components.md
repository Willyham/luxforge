# UI components for modules

Status: first slice implemented and verified on the M4 Mac. The five accepted decisions remain as recorded in [product decisions](../decisions.md#ui-components). The delivered widgets are listed in the [Develop workspace](develop-workspace.md#architecture); `text` and `pad` remain the second slice. The `string` parameter kind exists for the [presets module](presets.md#the-presets-module), whose strings no text field edits. This is the contract for the closed set of controls a tool module may declare and the widgets the desktop renders them with. It exists so that Tone Curve, Detail, the colour mixer and every later module can be generated from a descriptor without a desktop change, and so that the widget crate stays pure. The task plan is [UI components](../../tasks/implementation-ui-components.json).

## The three vocabularies

A "component" is three things that must be kept apart, because they live in different crates with compile-enforced boundaries.

| Layer | Crate | Knows about | Never knows about |
| --- | --- | --- | --- |
| **Control kind** | `lightwell-core::modules::descriptor` | Actions, parameters, ranges, defaults, hints. Validated at registration, listed by `module.list` | Pixels, layout, colours, Iced |
| **View model** | `lightwell-app/src/state/` | One control kind plus the displayed entry's values, drafts, field text, availability | Iced |
| **Widget** | `lightwell-ui` | A plain-data model and message callbacks | `lightwell-core`, parameters, actions, validation, requests |

A module declares a control kind bound to a parameter of one of its actions. The desktop chooses the widget and its states from the parameter's kind and hints. The widget draws what it is given. The rule of this design is that **nothing moves between these layers**: a widget takes formatted strings and clamped numbers, never a range to enforce; a control kind carries semantics, never a pixel size or a colour.

## Design goals against Lightroom and darktable

Both references are read as [research](../research), not as targets.

| Concern | Lightroom | darktable | Lightwell |
| --- | --- | --- | --- |
| Range | One hard range per slider; a typed value is clamped | Soft range on the rail, hard range for typed values, adjustable per slider | Declared soft and hard ranges: the rail spans the soft range, typing reaches the hard range, and a value outside the soft range is shown on the rail as an over-range mark, never hidden |
| Fine control | Option-drag and arrow keys | Right-click opens a fine slider; scroll wheel changes values | Arrow keys step, Shift steps ×10, Option steps ÷10, all declared by the parameter. No scroll-wheel editing: a trackpad scroll over a panel of sliders must never edit a photograph ([decision 1](#decisions)) |
| Reset | Double-click a label or a slider | Double-click a slider | Double-click the label or the slider, a group reset and a module reset, each an ordinary declared action |
| Value entry | Click the value to type | Right-click to type | Click the value to type; invalid text stays editable with the range shown and commits nothing |
| What a control is | GUI only; the SDK exposes a subset | Parameter blobs and Lua | Every control is one declared parameter of one API action; its JSON request is one gesture away and its parameter name is visible in the control menu |
| Disabled | Greyed, unexplained | Greyed or hidden | Disabled with the core's reason on the header or in the status bar; never hidden because of state |
| Curves | Parametric region sliders and a point curve, per channel | Many curve editors with different rules | One point-curve editor with a numeric point list beside it, so a person drags and an agent posts the same points |
| Colour pickers | HSL rails, colour grading wheels | On-canvas pickers, few dialog pickers | A swatch that opens a picker with a plane, a hue rail and fields; a picker drag drafts exactly as a slider does |

The improvements are all of one kind: **the same control is exact, typed, keyboard-reachable and programmable, with no gesture that only the pointer can perform.**

## Control kinds

The closed set a module may declare. Names are the JSON `kind` in `controls[]`. Every kind binds to an action and, except `action` and `picker`, to one or more of that action's parameters; registration rejects a binding whose parameter kind does not match. `picker` binds to the module's canvas declaration instead. Everything that is a `style` or a `hint` may be omitted and has the stated default, so a module that declares nothing beyond a label gets a sensible control.

| Kind | Binds to | Widget | Gesture and commit |
| --- | --- | --- | --- |
| `group {label, controls[], reset?, collapsed?}` | — | Sub-group header and a column | None. `collapsed` is the initial state only |
| `number {action, parameter, label, style?, rail?}` | `integer` or `number` | `style: slider` (default): the slider row. `style: field`: a labelled value field alone, for a value with no useful rail (a pixel coordinate, a radius in pixels). `style: stepper`: a field with minus and plus buttons stepping by `step`, for an angle or a count | Slider and stepper of a field-patch action draft on every change and commit once on release, key-up or Enter. A field commits on Enter. Escape cancels |
| `toggle {action, parameter, label}` | `boolean` (new) | A checkbox row: label and box | Commits once on click or Space |
| `choice {action, parameter, label, style?}` | `enum` | `style: segmented` (default up to four options), `chips` (default above four), `menu` (a drop-down, for long lists) | Commits once on selection |
| `color {action, parameter, label, style?}` | `color` | `style: fields` (default, today's three fields with a swatch) or `picker`: the swatch opens a picker with a saturation-value plane, a hue rail, three fields and a hex field | Fields commit on Enter; a plane or rail drag drafts as a slider does and commits once on release |
| `curve {action, channels[{parameter, label}], label, sample_query, background?}` | `curve` (new), one per channel | The curve editor: a square plot with a grid, the identity diagonal, the interpolated curve, its points and the selected point; a segmented channel switch when more than one channel is declared; a numeric point list under the plot. `background: none` (default) or `histogram`: the host draws the inspector's current normalized histogram behind the curve | Dragging a point drafts and commits on release; arrow keys nudge the selected point by `step`; Enter in the point list commits; Delete removes the selected point; double-click on the plot adds one. Channel selection is view state and changes no recipe |
| `action {action, label, preset?, style?, icon?}` | any action | `style: default`, `primary` (accent, one per surface) or `icon` (an icon button whose tooltip is the label). `icon` names one of the host's named icons; an unknown name renders the label | Runs the action once with the preset |
| `text {action, parameter, label, placeholder?}` | `string` (new) | A labelled text field | Commits on Enter |
| `pad {action, x, y, label, style?}` | two `number` parameters | A 2D pad; `style: plane` (default) or `hue-saturation` (a colour wheel: angle is `x` in degrees, radius is `y`) | Drafts on drag, commits once on release; arrows nudge |
| `picker {label}` | the module's own `point-pick` or `sample-apply` canvas, not a parameter | A button in the module's own panel, beside the controls its pick fills; it reads selected while that mode is active, and its tooltip names the mode and its letter | Enters the mode through `workspace.set`, and leaves it for the pointer when it is already selected. It commits nothing: the pick itself is what the canvas declares |

A module declares at most one `picker`, a `picker` needs a pick canvas (a `crop-frame` takes the whole photograph over and has its own controls, so it is not a pick), and a module that declares a pick canvas declares exactly one `picker`, so no pick mode is reachable only by its letter. Registration refuses each of the three.

`text` and `pad` are specified here so the vocabulary is closed, but they are the second slice: no built-in module needs them before the colour mixer's grading wheels. The core's `string` kind carries the presets module's name and library identity, which a client sends from the library rather than a text field. They are not built before then ([decision 2](#decisions)).

Kinds deliberately left out, with the reason:

- **Note or static text.** A section hint and parameter notes already exist. A module that wants prose in the panel is describing a control badly.
- **Equalizer or band sliders.** A row of bands is a `curve` whose points have fixed `x`; the curve kind carries a `fixed_x` list for exactly this, so no second editor is needed.
- **Histogram or plot of a query result.** The histogram is a core inspector, not a module control. A module that wants to show its own transfer function declares a `curve` with the channel bound to its parameter; a read-only plot is a later question, not a placeholder now.
- **Layer enable, mask, opacity.** These are host concepts on the recipe, not module controls. They arrive with their own designs.
- **Tabs, popovers and windows.** Layout belongs to the desktop. A module has groups and nothing else.
- **A second way into a canvas mode.** A module's pick mode has exactly one `picker`, its declared letter and the command palette. A kind that declared a mode a module does not own, or a second button for the same mode, would give the panel two selected states to reconcile.

### Parameter kinds and hints

The implemented additions are boolean, curve, string and points, plus numeric hints. No widget edits a string yet: that is the `text` control of the second slice, and **no widget edits a `points` path at all** — a path is drawn on the canvas, so `points` is a parameter kind with no control kind and the control vocabulary stays closed. The host validates and stores exactly what it is sent, as today; hints change what a client draws and never what the host accepts.

| Kind | Shape | Validation at registration and on every request |
| --- | --- | --- |
| `boolean` | `true` or `false` | Exactly a JSON boolean |
| `string {max_length}` | UTF-8 text | At most `max_length` characters, `max_length` at most 256, no control characters |
| `curve {points_min, points_max, monotone, fixed_x?}` | `[[x, y], …]` with `x` and `y` in `0..=1`, strictly increasing `x` | Between `points_min` (at least 2) and `points_max` (at most 32) points; `monotone` requires non-decreasing `y`; `fixed_x` requires exactly those `x` values in that order. Interpolation is the module's contract, stated in its notes, and the core exposes the module's sampled curve through a declared query, never through the widget |
| `points {points_min, points_max}` | `[[x, y], …]` in content-stage normalized coordinates, in drawn order | Between `points_min` (at least 1) and `points_max` (at most 1024, the declared points-per-stroke limit) positions, each two finite numbers within `-1..=2`; unlike a curve it is a path, so positions are unsorted and may repeat or reverse. An over-count is a `resource-limit` error naming the limit. The stored coordinate grid, the decimation contract and the content-addressed store a path is kept in are the host's [path primitives](masking.md#storing-a-path), published by `schema.list` under `paths`, and belong to no one feature |

`number` and `integer` parameters gain: `soft_min` and `soft_max` (the rail's range, inside the hard range; default the hard range), `fine_step` (Option-arrow; default `step / 10`), and `zero` (the tick the fill grows from; default `0` when inside the range and otherwise unipolar from the minimum). The existing `step`, `precision` and `unit` stay. A `number` control's `rail` hint decorates the rail: `plain` (default), `hue` (the spectrum), `temperature` (blue to amber), `tint` (green to magenta) or `gradient {stops: [color, …]}` for a module's own scale. The widget receives colour stops and nothing else, so a mislabelled rail can only be ugly, never wrong.

A curve control names its module-owned query in `sample_query`. The query declares each channel with the same curve kind, optional and without a default. The desktop sends `asset_id`, `entry_id` and only the active channel's points. The current result shape is `{ "points": [[x, y], …] }`: 2–1024 finite fraction pairs with strictly increasing x. The host neither interpolates nor substitutes the control points for a missing sampled result. It retains one active query and one replaceable pending query; asset, entry, channel, points and request identity reject stale results. Picker HSV state preserves a chosen hue through grey or black while its associated RGB value remains authoritative.

### Icons

Icon buttons name an icon; they never embed one. `lightwell-ui` owns one `Icon` enumeration (rotate-left, rotate-right, flip, mirror, crop, picker, reset, plus, minus, lock, swap, guide, pointer, and the ones the state panel and title bar already use) and draws each as a vector path in the text colour, replacing today's Unicode glyphs. The core validates only that an icon name is a lowercase hyphenated name; the desktop maps it and falls back to the label. This keeps the core free of a UI asset list and the widget crate free of a registry.

## Widget library

Every widget is a function from a plain-data model and messages to an `Element`, holds no state and validates nothing, exactly as the existing thirteen do. The additions and changes, with what each takes:

| Widget | Model | What it emits |
| --- | --- | --- |
| `slider` (changed) | Adds `soft_min`, `soft_max`, `over_range: Option<Side>`, `rail: RailDecoration` (plain or colour stops), `fine_step` | Unchanged |
| `number_field` (new) | Label, `ValueEdit`, unit, enabled, invalid message. Factored out of the slider's value field so the slider and the stepper share it | Edit start, text, submit, reset |
| `stepper` (new) | A `number_field` plus minus and plus enablement and their tooltips, and an optional rail (soft range, value, step, zero, dragging) drawn between minus and plus by the slider's own rail line, so its geometry, fill, handle and halo are the slider's | Decrement, increment, the field's messages, and the rail's fractions and release |
| `toggle` (new) | Label, `on`, enabled | Toggled |
| `menu_choice` (new) | Options, selected index, enabled; wraps Iced's pick list in the theme | Selected index |
| `color_swatch` (new) | An `[u8; 3]`, enabled, `open` | Press |
| `color_picker` (new) | Hue, saturation, value as fractions, the swatch, three channel `ValueEdit`s and a hex `ValueEdit`, `dragging` | Plane position as fractions, hue fraction, release, field texts, submit, reset |
| `curve_editor` (new) | Points as fractions, selected point, the sampled curve as a polyline of fractions supplied by the host, optional background of 256 normalized heights, the identity flag, channel labels and the selected channel, `dragging`, enabled | Point moved to a fraction pair, point selected, point added at a fraction pair, point removed, release, channel selected |
| `pad` (new, second slice) | Two fractions, style, `dragging` | Position as fractions, release |
| `text_field` (new, second slice) | Label, text, placeholder, invalid message, enabled | Text, submit |
| `icon` (new) | An `Icon` and a size | Nothing; a drawing |
| `double_click` (delivered) | Any content and one message | That message on the second click of a run over the content, which it does not forward. The slider already wraps its rail in it, which is what makes a double-click on the rail reset the field: iced's slider captures the press itself, so a `mouse_area` around it never sees one |
| `truncated_text` (delivered) | A string, size, font and colour | Nothing; one line that ends in "…" at the longest character-boundary prefix that fits the width its row leaves it. A collapsed band's hint or unavailable reason, a history row's label (the actor keeps its full width) and a recipe row's summary use it. The prefix search is a pure function over a width-measuring closure, tested without a renderer, and the measurement is cached against the content, width, size and font |

Fractions everywhere: a widget maps pointer positions to `0..=1` on its own axes and the view model maps fractions to values with the parameter's range, step and precision. The pure mapping functions (`geometry::value_from_fraction` today, the picker's HSV and hex conversions, the curve editor's hit test and the pad's polar mapping) are tested without a renderer. Canvas-drawn widgets (the curve editor, the picker plane, the pad, icons) cache their tessellated geometry the way the histogram plot does, keyed on the model's version, so an unchanged model costs no re-tessellation per frame.

The editable input inside `number_field` is also used by the RGB/hex fields, curve coordinates and zoom; the curve channel selector uses the shared segmented control. This input primitive adds no descriptor-level text control.

The components board gains a row per new widget and state, `gallery_states()` builds every one, and a `gallery` smoke scenario renders the gallery in the real app so the board and the build are compared with correlated captures rather than by eye alone.

## Developer gallery

The title bar exposes **Developer** in debug builds and in optimized builds started with
`--developer`. It opens the existing 74 reference states across ten component pages, using the
same widget constructors as the editor. A page menu and Previous/Next buttons browse the board;
Back to editor or Escape restores the workspace. No photograph is required. The examples display
reference states and do not edit the photograph; the Controls proof provides live editing tests.
Opening is disabled during an active draft, import or Compare hold. Photo shortcuts are suppressed
while the board is shown. The editor retains its photo, recipe, selection, panels and controls.

Navigation uses the shared command service: `workspace.set {"component_gallery": 0}` opens the
first page, pages 0–9 select a page, and `{"component_gallery": null}` returns to the editor.
Omitting the field preserves it; invalid pages are refused atomically. `session.state` reports this
per-client preference. The API accepts the preference independently of the developer UI flag;
normal optimized desktops do not show the board. Navigation creates no edit, preview job or timer.
The gallery smoke uses these same navigation messages and verifies the return to the editor.

## Desktop mapping and gesture rules

The generated tools panel maps a declared control to a view model with the rules the workspace design already fixes, extended one kind at a time:

- **Continuous controls draft; discrete controls commit.** Slider, stepper drag, colour picker plane and rail, curve point drag and pad are continuous: a control whose field is a whole request (a field-patch action's field or an action's only parameter) opens one core draft on the first change, sends at most one `draft.set` and one preview per 16 ms tick, and commits once on release. Field, toggle, choice, colour fields, text and action commit once. A control of a non-patch action with multiple parameters sends the whole action on commit, as the crop and pixel controls do today.
- **One field per request.** A control of a patch action, or an action declaring that parameter alone, submits its own parameter only; a curve channel submits its one curve parameter; a pad submits its two. An action button bound to a patch action declares exactly one preset field, enforced at registration. A reset submits its declared preset only.
- **Authoritative values.** Every generated field is seeded from the displayed entry's values after each refresh, except the field being edited. Original and Custom on a sub-group are derived from declared defaults exactly as today, and a `curve` is Original when it equals its default points.
- **Every control has the menu.** Right-click on any generated control offers Copy as JSON request, with the current expected revision, and shows the action and parameter names. The curve editor's request carries the whole point list; a picker's request is the `workspace.set` its click would send.
- **Disabled means explained.** A control is disabled only when the core disables the section, and the section names the reason. No control is hidden because of state.
- **Keyboard.** Tab order follows control order. A focused slider, stepper or field steps with arrows, Shift ×10 and Option ÷10; a curve editor moves its selected point; a toggle flips on Space; a choice moves on arrows. Iced's slider takes arrows only while the pointer is over its rail, as recorded in the workspace design, and that limitation is stated, not hidden.

The crop section keeps its own draft and panel. Its ratio chips, custom ratio fields, angle stepper and straighten toggle are rebuilt on the generic `chip`, `number_field`, `stepper` and `toggle` widgets so the desktop has one implementation of each, but its messages stay crop's own because its draft is desktop-local. The angle's rail is a stepper drag: each move sets the draft's angle on the rail's 0.05° step (the Option nudge, a tenth of the 0.5° button step, since the crop descriptor declares no step for its angle) and refits the rectangle, and only the release is a draft change. The rail's handle is accent for as long as the draft is open, as the crop reference draws it. The value box shows the angle with its `°` and opens for typing when pressed; the descriptor declares the unit as `deg` and no precision, so the angle reads `2.4°`, not the reference's `2.40°`.

## Controls proof module

A developer test module `lightwell.controls` declares one control of every kind bound to one field-patch action `set-controls` with a parameter of every kind, an `action` in each style with an icon, and a `curve` with two channels and a histogram background, and compiles to a no-op colour stage. It is registered and listed only in developer mode (automatic in debug builds, `--developer` in optimized builds); outside that mode both `module.list` and the workspace omit it. The existing pixel proof remains in the built-in API registry and is shown in the workspace only in developer mode. `edit.set-controls` accepts exactly one field; the module and group use the separate non-patch `edit.reset-controls` action. `query.sample-controls-curve` returns 257 piecewise-linear samples. The identity layer shares the source pixel allocation. It is how UI/API parity is proven for the vocabulary as a whole rather than for whichever module happens to use a kind first: every control's message produces the request an independent JSON client sends, and every request the client sends is reflected in the control.

## Verification and acceptance

- **Core.** Registration rejects each new mismatched binding (a toggle on a number, a choice on a boolean, a curve on a colour, a rail hint on a colour, soft bounds outside the hard range, a fine step that is not positive, more channels than parameters); accepts each valid one; `module.list` shows every kind and hint; every new parameter kind validates requests and refuses bad ones with the field named.
- **Widgets.** Pure mapping functions with tests: soft-range fill and over-range side, HSV and hex conversions both ways, curve hit test and fraction rounding. The gallery builds every state without panicking and links no `lightwell-core`. `cargo xtask check-repository` keeps the layer boundary.
- **Desktop.** Parity for every control of the proof module through the JSON method table; per-kind gesture tests: a slider or curve drag opens one draft and commits once, a toggle sends one boolean, a choice sends one option, a picker drag drafts, a field commits on Enter and not on blur, Escape cancels each continuous gesture. Per-section re-derivation still keeps an untouched section's version.
- **Rendered evidence.** A `gallery` smoke scenario and a `controls` smoke scenario over the proof module, captured with correlated state and logs on the M4 Mac, are compared with the components board. The 24 MP slider-to-frame latency measured by `cargo xtask editor-latency` does not regress with the rail decoration or the factored value field, and a curve point drag meets the same provisional threshold as a slider drag.
- **Docs.** The workspace design's widget row, [feature status](../features.md) and the [user guide](../user-guide.md) describe the delivered kinds; the module design's descriptor table lists them.

Native gallery and controls captures pass, and the matched 24 MP slider and curve drag measurements meet the provisional p95 target; scope, retained misses and core timings are in [performance qualification](../specs/performance.md#ui-components-qualification).

Acceptance is that the Tone Curve, Detail and colour mixer modules can be declared against this vocabulary with no widget or desktop change, demonstrated by the proof module before any of them is written.

## Decisions

Accepted by the owner on 2026-09-21 and recorded in [product decisions](../decisions.md#ui-components).

1. **Scroll-wheel editing is off**, with no preference to turn it on in v0. darktable users expect it; the cost is accidental edits on a trackpad, which the history model makes recoverable but not invisible.
2. **Second slice.** `text`, `pad` and the `string` kind wait for the colour mixer design. `curve` channels ship with the kind, since the editor's channel switch is cheap and the proof module exercises it, even though Tone Curve may start composite-only.
3. **Vector icons now.** The Unicode glyphs are replaced with canvas-drawn paths in this work, because icon buttons become a declared style.
4. **Curve interpolation is the module's.** It is sampled through a declared query, so the widget stays pure and two modules may choose different splines. A host-owned monotone cubic for every curve was rejected because it would make a module contract out of a host choice.
5. **Option is the fine-step modifier**, ÷10, alongside the existing Shift ×10. Lightroom uses Option-drag for clipping preview, so a Lightroom user will not miss it; the modifier can be swapped later without a descriptor change.
