# Develop workspace

Status: implemented and verified on the M4 Mac for the shell, the widget library, the generated panels and the canvas modes over the modules that exist today (pixel, transform, crop), under the [architecture](#architecture) below. The histogram and the Basic, export, Locate, heal and mask tools the mockups also show are not built; their rows in the tool array say so. This is the design for the single-image editing screen: what it shows, how the tools are arranged and how they behave. Library, catalog management and settings are out of scope. The owner's decisions are recorded in [decisions](#decisions) and in [product decisions](../decisions.md#develop-workspace).

## Mockup

Rendered at 1440 × 900 logical points, 2× scale, from the design artboards. The photograph is the owner's Sapa drone JPEG from the fixtures; the slider values, history and agent are illustrative. The boards include the histogram and the Basic section, which are not built; the delivered screen has the same regions, strip, bars and notices without them. Rendered evidence of the built screen comes from `cargo xtask smoke --scenario workspace` and `--scenario unavailable` (see [verification](#verification)).

| Board | Shows |
| --- | --- |
| [Default](develop-workspace/default.png) | The workspace with Basic expanded, history and recipe on the left, mode strip under the photo |
| [Crop draft](develop-workspace/crop-draft.png) | Crop mode: dimmed overlay, thirds, handles, the draft bar and the Crop section with the exact values it would commit |
| [Changed elsewhere](develop-workspace/changed-elsewhere.png) | An agent commits during a slider draft: the conflict notice, actor-attributed history and a control's Copy as JSON request menu |
| [Components](develop-workspace/components.png) | Tokens, slider states, module headers, history rows, canvas modes, notices and the command palette |

![Develop workspace, default state](develop-workspace/default.png)

## Principles

- **The photograph is the only colour on screen.** Chrome is neutral grey in a few tonal steps; one warm accent marks the current state and the active mode; red and blue are reserved for clipping.
- **State on the left, tools on the right.** The left panel answers "what has happened to this photo" (history, versions, the recipe). The right panel answers "what can I do" (histogram, modules). The canvas between them is the photograph and nothing else until a mode needs an overlay.
- **Adjustments and modes are different things.** An adjustment module (Basic, later Tone curve, Detail) is a stack of controls that commit on release. A canvas mode (Crop, later Heal and Mask) takes over the canvas, runs a draft and ends with Apply or Cancel. The screen shows which kind is active and never mixes their commit rules.
- **Every visible control is an API call.** The panel is generated from module descriptors, history rows name their action and actor, and any control can hand you its JSON request. Nothing in the design is a GUI-only gesture.
- **Nothing is hidden and nothing is modal.** Unavailable providers, stale previews, conflicts and missing originals appear as notices over the canvas with the explicit actions the core allows. Editing is disabled only when the core disables it, and the status bar says why.

## Layout

At the reference size (1440 × 900 logical points on the M4 MacBook Pro) the screen has five regions. The two side panels collapse independently; the canvas takes whatever remains.

| Region | Size | Contents |
| --- | --- | --- |
| Title bar | 44 pt | File name and dimensions, Open; centred view control (Fit, 100%, typed percentage) and Compare; Undo, Redo and the two panel toggles. Clipping and Export arrive with their own features |
| State panel (left) | 240 pt | Versions as chips; History as one row per entry (sequence, marker, action label, actor); Recipe as the ordered layer stack |
| Canvas | remainder | Photograph centred at Fit or scrolled at a percentage, on the darkest surface; a floating mode strip at the bottom; notices and a draft bar at the top |
| Tools panel (right) | 300 pt | One collapsible section per registered module, then a Developer section when `--developer` lists test modules. The histogram sits above the modules once the [Basic and histogram](basic-and-histogram.md) work lands |
| Status bar | 26 pt | Status message with Copy; connected clients; render state and time; zoom and display scale |

The mode strip holds the pointer, one entry per registered module that declares a canvas interaction (crop today; the neutral picker, heal and mask when their modules exist) and the view overlays (thirds). It floats over the canvas so it stays next to the photograph when the panels are hidden.

## Tool array

Where each tool lives, what kind of thing it is and whether it exists. Kinds: **module** sections render from descriptors in registry order; **mode** entries come from a module's canvas declaration; **view** and **core** items are host features that need no module.

| Tool | Where | Kind | Status | Programmable through |
| --- | --- | --- | --- | --- |
| Fit, 100%, typed zoom, pan | Title bar, Space-drag | view | Implemented | `view.set` |
| Compare with original (hold `\`) | Title bar | view | Implemented | `preview.select` and `preview.return-current` on the Original entry; refused while a draft is open |
| Histogram, clipping triangles and overlays, RGB readout | Top of tools panel | core inspector | Proposed ([Basic and histogram](basic-and-histogram.md)) | Histogram and overlay capabilities in that design; readout via `render.sample` |
| Basic: White balance (Temperature, Tint, Original/Custom, neutral picker), Tone (Exposure, Contrast, Highlights, Shadows, Whites, Blacks), Color (Vibrance, Saturation) | Tools panel, first section | module + picker mode | Proposed ([Basic and histogram](basic-and-histogram.md)) | `edit.set-basic` and the draft lifecycle in that design |
| Transform: Rotate left/right, Mirror, Flip | Tools panel | module | Implemented (M2, M3) | `edit.transform` |
| Crop and straighten: free handles, ratio presets, lock and swap, angle, straighten guide, reset | Mode strip, `R`; Crop section while drafting | mode | Implemented (M4) | `edit.crop`, `edit.crop-fit`, `edit.crop-reset` |
| Tone curve | Tools panel, collapsed | module | Later | Its own design |
| Color: mixer and B&W | Tools panel, collapsed | module | Later | Its own design |
| Detail: sharpening, noise reduction | Tools panel, collapsed | module | Later | Its own design |
| Effects: Texture, Clarity, Dehaze | Tools panel, collapsed | module | Later | Its own design |
| Heal, Mask | Mode strip | mode | Later | Their own designs |
| History, Undo, Redo, preview, Restore, Load older | State panel, title bar, shortcuts | core | Implemented (M1) | `history.list`, `history.inspect`, `history.undo`, `history.redo`, `history.restore`, `preview.select` |
| Versions: save, select, delete | State panel | core | Implemented | `version.create`, `version.list`, `version.delete` |
| Recipe (layer stack) | State panel | core | Implemented as rows | `recipe.describe` |
| Export | Title bar | core | Editor follow-up | The export design |
| Locate | Notice on a missing original | core | Editor follow-up | [Source recovery](../specs/source-recovery.md) |
| Command palette (`Cmd+K`) | Overlay | core | Implemented | Every listed module's `module.list` controls, resets and canvas modes, plus the host view, history and panel commands |
| Copy as JSON request | Control context menu, the crop draft's Apply | core | Implemented | The control's generated action, or `edit.crop` for the open draft |
| Show in schema | Control context menu | core | Later | Its own design |
| Pixel proof | Developer section, listed only with `--developer` | module | Implemented (M1) | `edit.set-pixel` |

A build shows only the modules its registry contains. Sections for later modules exist in the mockup to settle their place in the panel, not as placeholders in code; the desktop never lists a module that is not registered. The pixel proof tool is a test module: its descriptor is marked `developer` and the desktop lists it under a Developer section only when launched with `--developer`, so the default workspace stays a photo editor. `--disable-module ID` registers a built-in as unavailable, which is how the unavailable-provider state is demonstrated.

## Panels

### State panel

- **Versions** are chips: name and entry number. The chip for the displayed entry is tinted. Selecting one previews that entry; the plus button reveals a name field and Save for the displayed state. Delete is in the chip's context menu, an inline menu under the chips.
- **History** lists newest first: sequence number, a marker, the stored label and the actor. The label is the action's rendered `summary` template when the module declares one ("Crop 16:9", "Rotate right", "Pixel 12, 34") and the action title otherwise. Markers: filled accent for the current entry, an accent outline for a previewed entry, hollow for the rest. Undone entries stay in the list, dimmed and labelled branch. Load older appears when a page remains.
- **Recipe** lists the layer stack in stored order with the module title and the module's own payload summary from `recipe.describe` ("Rotate right", "61% × 80% at 3.5°", "Whole image"); a layer whose provider is unavailable shows the reason instead. It is the durable order of processing, which is why it lives with history and not with the tools: the panel order on the right never changes it.
- During a historical preview the panel shows Return to current and Restore under the list and the tools panel is disabled with its values still visible, matching the current behaviour.

### Tools panel

- The **histogram** is not built. When the [Basic and histogram](basic-and-histogram.md) work lands it sits above the modules with no header, its caption names the domain ("Output · sRGB · after crop"), the pointer readout shows the three output codes and the two triangles toggle the shadow and highlight overlays.
- Each **module section** has a header row: disclosure, title, an accent dot when the module has a non-neutral layer in the current recipe, and the module's declared reset action. Collapsed headers show the descriptor's hint. Unavailable modules show their reason in place of the hint and cannot expand. Section order follows the registry; a disabled section (historical preview, request in flight, no photograph) keeps its values visible and names the reason.
- **Sub-groups** inside a module come from the descriptor's `group` controls and carry their own reset when the group declares one. The generated mapping is: `number` and `integer` to a slider, `enum` to a segmented control (up to four options) or chips, `color` to three fields, `group` to a sub-group, `action` to a button, a `canvas` declaration to a mode strip entry; any other kind renders an explicit unsupported message.
- A **slider** is a label, a right-aligned value that becomes a text field when clicked, and a 2 px track with a fill growing from the zero tick for bipolar ranges. The thumb turns accent while it is dragged. A drag changes only the field and sends nothing; release, key-up or Enter runs the control's action once. Invalid text stays editable with the declared range shown under the field and commits nothing. Double-click a label resets one field to its default. Values are right-aligned in a fixed box because Iced cannot request tabular numerals.
- **Transforms** renders its four declared actions as buttons. The **Crop and straighten** section shows one Crop and straighten button that enters the mode; while a draft is open the section is held expanded with the ratio chips, custom ratio, lock and swap, angle and nudges, straighten guide, Apply, Cancel and the draft's exact values, and it collapses again as the user left it on Apply or Cancel.

### Canvas

- At Fit the photograph is centred with 20 pt of surface around it; at a percentage it scrolls on both axes and Space-drag pans. 100% keeps its physical-pixel meaning.
- A **draft bar** appears at the top of the canvas while a mode has a draft: mode name, a one-line readout, Cancel (Escape) and Apply (Enter). The same values appear in the mode's section for keyboard editing.
- The crop overlay is the current one: dimmed outside, thirds, border and eight handles, with the crop layer's input stage shown underneath.
- **Notices** are cards at the top of the canvas: Changed elsewhere (Discard, Reapply), Preview is stale (the unavailable module by title and its reason), Original not found and rendering limits (the reason, no action until Locate exists). A photograph that cannot render shows "Preview unavailable" and the reason in place of the pixels. Notices never block the rest of the screen.
- Compare holds the Original entry's preview while `\` or the Compare button is down and releases it back to the previous selection without touching history or session state beyond the preview. It is refused while a crop draft is open, because a preview would pause the draft.

## Interaction rules

- **Commit timing.** Adjustment sliders commit once on release, key-up or Enter; Escape cancels and focus loss cancels an unfinished gesture. Canvas modes commit only on Apply. Both follow the draft lifecycle from the [Basic and histogram design](basic-and-histogram.md) and the [crop contract](../specs/single-image.md).
- **One draft per client and asset.** Leaving the crop mode, or starting Compare, with a draft open is refused with the reason in the status bar; nothing discards a draft silently. Starting a draft by any route sets the session's canvas mode to the module and ending it returns to the pointer.
- **Live agents.** An external commit updates history and the canvas immediately. If the client has a draft the draft is kept, the conflict notice appears and Apply is refused until Discard or Reapply, matching the accepted M4 behaviour. Every history row shows its actor, and the status bar counts connected clients.
- **Copy as JSON request** on any generated control (right-click, then the inline menu) writes the exact `edit.<action>` request for the control's current values, with the current expected revision, so a person can hand an agent what they just did. The crop draft's Apply offers its `edit.crop` request the same way.
- **Keyboard.** Cmd+O, Cmd+Z, Shift+Cmd+Z, Tab and Shift+Tab between fields, Enter and Escape in a draft, Space-drag, `F` Fit, `1` 100%, `\` compare (held), `O` thirds, `V` pointer, each module's declared letter (`R` crop), Cmd+K command palette, Cmd+Option+[ and ] toggle the side panels, Up and Down and Enter in the palette. Letters act only when no text field has focus; the keymap is one table in `app/keymap.rs`. `J` clipping, `W` neutral picker and Cmd+E export arrive with their features. Arrow keys on a focused slider are Iced's own stepping and are not covered by rendered evidence.

## Visual language

| Token | Value | Use |
| --- | --- | --- |
| Canvas | `#19191b` | The darkest surface; the photograph sits on it |
| Panel | `#202023` | Side panels and status bar |
| Bar | `#232326` | Title bar, floating strips, notices |
| Control | `#2c2c31` | Buttons, chips, text fields |
| Border | 6% white | All dividers and outlines |
| Text | `#e8e8ea` / `#a8a8ae` / `#77777f` | Primary, secondary, tertiary |
| Accent | `#e2b46a` | Current entry, active mode, non-neutral dot, dragging thumb, Apply |
| Clipping | `#e5534b` / `#4c8be0` | Highlight and shadow indicators and overlays only |

System UI face (SF Pro on macOS, the platform default elsewhere), 12 pt controls, 13 pt semibold titles, 11 pt captions, 10.5 pt capitalised section labels. Tabular numerals are not available through Iced, so values are right-aligned in a fixed-width box instead. 8 pt spacing grid, 6 pt radii, 1 px borders, no gradients or blur; a 30% white guide colour for the thirds overlay. Invalid values and unavailable reasons use the clipping red, as the components board draws them. Dark only; a light theme is not planned. The tokens live in `crates/lightwell-ui/src/theme.rs` with a test per value.

## What the desktop needs from the core

The screen is renderable from today's descriptors with a small set of additions, each visible through `module.list` or `session.state`: section hints, reset actions, history summary templates, per-layer summaries, canvas titles and shortcuts, the developer flag and per-client workspace state. They are specified in [core additions](#core-additions) below. Compare, the command palette and Copy as JSON request need no core change.

Widgets keep no authoritative state. Panel collapse, mode and overlay toggles are per-client session state, reported with `session.state` like zoom today. The tools panel refreshes only the section whose values changed, and slider drags produce one preview request per frame at most, per the [performance rules](../engineering/performance-rules.md).

## Verification

What is demonstrated, and how, on the owner's M4 Mac (release build, Metal, background bundle launches):

- `cargo xtask smoke --scenario workspace` opens a fixture at 1440 × 900 and captures eleven frames: the default screen, a transform, each panel collapsed, the thirds overlay, a historical preview and its return, a crop draft, a commit during that draft (the Changed elsewhere notice), the command palette and the cancelled draft. Each frame's `state` records the session workspace state, notices, revision, displayed generation and the draft, and the runner checks them together with the photograph's placement inside `surface_columns`.
- `cargo xtask smoke --scenario unavailable` reuses a catalog holding a crop layer under `--disable-module lightwell.crop` and checks the Preview is stale notice, the unavailable section and the untouched source.
- `crop` and `crop-draft` keep proving the overlay, handles, presets and committed stacks on the new layout; `load`, `empty`, `replacement`, `invalid`, `repeated`, `alternating` and `large24` keep proving opening and failure retention.
- Every generated control is driven from `module.list`; the app tests prove each control's message builds the request an independent JSON client sends and that a slider drag sends nothing until release.
- `cargo xtask measure` (10 samples per workload, release build, M4 Pro, Metal, background bundle launches, warm filesystem cache) before and after the work. Launch-to-first-frame medians: empty 573 to 561 ms, 24 MP 627 to 607 ms, 60 MP 740 to 723 ms; request-to-capture medians 151 to 134, 199 to 185 and 291 to 276 ms. Sampled peak RSS is a 50 ms sampling range over a process that lives under a second, not a steady idle figure: the empty window's samples span 100 to 142 MiB after against 100 to 122 before, and the 60 MP peak reads 1005 MiB in three of ten runs after against 965 MiB in every run before, with the other seven runs unchanged. That intermittent 40 MiB at 60 MP is not diagnosed and is an open item, not a claimed pass.

Not demonstrated: the histogram (not built); keyboard-only reach of every control (Tab walks the generated fields, the palette runs every action, but no rendered check walks the whole screen by keyboard); the cost of expanding a section, which allocates nothing today because no module declares a lazy resource; native Windows and Linux rendering.

## Decisions

Accepted by the owner on 2026-09-20; see [product decisions](../decisions.md#develop-workspace).

| Decision | Decided | Effect |
| --- | --- | --- |
| Panel split | State left, tools right, both collapsible | History stays next to the photo and state never mixes with tools |
| Module presentation | Stacked collapsible sections in registry order | The recipe's breadth stays visible and the panel keeps Lightroom familiarity |
| History label | Action title plus a one-value summary from the module, stored with the entry | Rows read as edits, not logs |
| Test modules | Developer section behind the `--developer` launch flag, hidden by default | The default workspace stays a photo editor; the API is unaffected |
| Compare | Hold `\` for the Original entry | No second render, no second layout |
| Light theme | Not now | One rendered-evidence surface |
| Scope of the first build | Shell, widget library, generated tools panel, state panel, canvas modes and bars for pixel, transform and crop | Basic, histogram, export, Locate, heal and mask are left out entirely, not drawn as placeholders |

## Architecture

The desktop is split into layers with hard boundaries. The boundaries are enforced by crate dependencies, by module structure and by the repository check, which refuses an `iced` import under `state/` and a `lightwell_core` import under `view/` or in the widget crate.

### Crates and modules

| Layer | Where | Depends on | Holds |
| --- | --- | --- | --- |
| Widget library | `crates/lightwell-ui` | Iced only, never `lightwell-core` | Theme tokens from the [visual language](#visual-language) and the widgets: bipolar slider with an editable value and draft, typing and invalid states, section header with dot and reset, sub-group header, icon button, segmented control, chip, list row, notice card, floating bar, mode strip, inline menu. Every widget takes plain data and callbacks and holds no editing logic |
| View model | `crates/lightwell-app/src/state/` | `lightwell-core` types and `serde_json`; no Iced | Pure functions from core state (asset state, history page, versions, lineage, descriptors, layer summaries, drafts, notices, session workspace state, field text) to plain-data models: title bar, state panel, tools panel sections, canvas (mode strip, draft bar, notices), status bar and command palette |
| View | `crates/lightwell-app/src/view/` | `lightwell-ui`, Iced and the view models; no `lightwell-core`, no owner calls | One file per region, each a pure function from a model to an `Element<Message>`: `title_bar`, `state_panel`, `canvas`, `tools_panel`, `status_bar`, `palette`, plus the layout that composes the five regions and the crop canvas program |
| Update and effects | `crates/lightwell-app/src/app/` | Everything above plus the owner handle | The Iced application: the `Editor` state, semantic messages, the update function, owner tasks, evidence mode and scripts, the keymap in one file, and the crop-draft driver |
| Draft state machines | `crates/lightwell-app/src/crop_draft.rs`, `crop_canvas.rs` | As today | The crop draft and its canvas program stay their own modules and are driven from `app/` |

The widget crate is a separate crate for two reasons. It cannot depend on `lightwell-core`, so a widget cannot reach authoritative state, validate a parameter or build a request: the compiler enforces the "widgets hold no logic" rule rather than a review. And it compiles in isolation, so a styling change rebuilds the widgets and the app, never the core. It is a compile-time boundary only: it links into the same binary, adds no startup work, and `cargo xtask measure` on the empty workload before and after the split shows no launch-to-frame or idle cost.

### Message flow

1. A gesture or key becomes one semantic `Message` in `app/`: "field `angle` of `crop` is now `3.5`", "submit action `transform` with preset rotate-right", "toggle the state panel", "enter mode `lightwell.crop`". Pixel deltas, pointer positions and key codes never leave the view or the keymap.
2. `Editor::update` either changes local UI state (field text, an open palette, a value being typed, a menu) or returns a task that calls the owner through the same methods the JSON API dispatches: `edit.<action>`, `history.*`, `preview.*`, `version.*`, `view.set`, `workspace.set`, `recipe.describe`. There is no desktop-only mutation path.
3. A response is adopted only if newer (session revision, event sequence), then the view models are re-derived. The tools panel keeps one model per module section with an input version; a field change, a draft change or a state change re-derives only the sections whose inputs changed, and a unit test proves an untouched section keeps its version.
4. A slider drag sends one field message per pointer move and no owner request; release, key-up or Enter sends the control's action once, exactly as Enter in a field does today. No module in this build previews during a drag, so a drag produces zero preview requests; when a module declares a draft-capable action the rule becomes at most one preview request per frame, and the update function is where that bound lives.
5. Crop drafts keep their M4 state machine: mode strip or `R` calls `workspace.set` for the mode and opens the draft; the draft bar and the Crop section drive `CropMessage`s; Apply sends `edit.crop`; an external revision marks the draft conflicted and the notice offers Discard or Reapply.

### Core additions

The additions below are the only core changes. Each is visible through `module.list` or `session.state` and covered by an API test.

| Need | Change |
| --- | --- |
| Section hints | `ModuleDescriptor.hint: Option<String>`, shown on a collapsed header |
| Reset actions | `ModuleDescriptor.reset` and `Control::Group.reset`, each an optional `{action, preset}` validated like an action control. The crop module declares `crop-reset` as its module reset; its former Reset crop button is the same action reached from the header |
| History labels | `ActionDescriptor.summary: Option<String>`, a template over the action's parameters (`{name}`). At commit the host renders it with the stored parameters (integers as written, numbers without trailing zeros, colours as `r,g,b`, enum options title-cased with hyphens as spaces) into `HistoryEntry.label`; without a template the label is the action title. The host labels Original and restore entries itself. Entry labels and typed source interpretation are persisted in current catalog format 4; unsupported catalog formats are refused explicitly |
| Recipe rows | `ToolModule::describe_layer` returns a short payload summary; the read-only method `recipe.describe {asset_id, entry_id?}` lists an entry's layers with module id, title, summary and availability. An unavailable provider's layers are listed with the reason instead of a summary |
| Mode strip | `CanvasInteraction` gains `title` and `shortcut` (one letter, unique across the registry, optional) on both variants |
| Developer section | `ModuleDescriptor.developer: bool`; the pixel module sets it. The desktop lists developer modules only with `--developer` |
| Workspace state | `ClientSession.workspace {state_panel, tools_panel, mode, thirds}` reported by `session.state` and set by `workspace.set` with optional fields; `mode` is `pointer` or an available module id that declares a canvas interaction |
| Evidence for an unavailable provider | `OwnerHandle::start_with` accepts a registry; the desktop's `--disable-module ID` flag registers that built-in wrapped as unavailable, so rendering a stack that uses it fails with the unavailable-effect error and the notice appears. `LocalServer::connected` reports the live client count for the status bar |

Compare, the command palette and Copy as JSON request need no core change.

### What is tested where

| Layer | Tests |
| --- | --- |
| `lightwell-core` | Descriptor validation for reset, summary, shortcut and developer; label rendering; `recipe.describe`; `workspace.set` and `session.state`; format 3 refusal of a format 2 catalog; every addition through the JSON method table |
| `lightwell-ui` | Token values equal the visual language; the slider's fill geometry, value-field states and the mapping from a pointer position to a value are pure functions with tests; no test links `lightwell-core` |
| `lightwell-app/src/state/` | History labels and markers, branch marking, disabled reasons, conflict state, which section is expanded, what a slider shows while its value is being typed or dragged, the mode strip entries, the notices, the palette entries, per-section re-derivation |
| `lightwell-app/src/app/` | Every generated control's message produces the request an independent JSON client would send (parity), the keymap table, evidence steps, draft driving, compare restore |
| `xtask` | The layer-boundary check in `check-repository`; smoke scenarios `workspace` (default, each panel collapsed, thirds, historical preview, conflict during a draft, palette, cancel), `crop`, `crop-draft` and `unavailable`; `measure` before and after |

