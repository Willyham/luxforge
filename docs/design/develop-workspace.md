# Develop workspace

Status: accepted for the shell, the widget library and the panels over the modules that exist today (pixel, transform, crop); implementation in progress under the [architecture](#architecture) below. This is the design for the single-image editing screen: what it shows, how the tools are arranged and how they behave. Library, catalog management and settings are out of scope. The tool list below marks what exists today against what is only planned. The owner's decisions are recorded in [decisions](#decisions) and in [product decisions](../decisions.md#develop-workspace).

## Mockup

Rendered at 1440 × 900 logical points, 2× scale, from the design artboards. The photograph is the owner's Sapa drone JPEG from the fixtures; the slider values, history and agent are illustrative.

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
| Title bar | 44 pt | File name and dimensions; centred view control (Fit, 100%, typed percentage), Compare and Clipping toggles; Undo, Redo, Export, panel toggles |
| State panel (left) | 240 pt | Versions as chips; History as one row per entry (sequence, marker, action label, actor); Recipe as the ordered layer stack |
| Canvas | remainder | Photograph centred at Fit or scrolled at a percentage, on the darkest surface; a floating mode strip at the bottom; notices and a draft bar at the top |
| Tools panel (right) | 300 pt | Histogram with clipping triangles and the pointer readout; then one collapsible section per registered module |
| Status bar | 26 pt | Status message with Copy; connected clients; render state and time; zoom and display scale |

The mode strip holds the canvas modes (pointer, crop, neutral picker, later heal and mask) and view overlays (thirds). It floats over the canvas so it stays next to the photograph when the panels are hidden.

## Tool array

Where each tool lives, what kind of thing it is and whether it exists. Kinds: **module** sections render from descriptors in registry order; **mode** entries come from a module's canvas declaration; **view** and **core** items are host features that need no module.

| Tool | Where | Kind | Status | Programmable through |
| --- | --- | --- | --- | --- |
| Fit, 100%, typed zoom, pan | Title bar, Space-drag | view | Implemented | `view.set` |
| Compare with original (hold `\`) | Title bar | view | Proposed | `preview.select` and `preview.return-current` on the Original entry |
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
| Recipe (layer stack) | State panel | core | Implemented as text; proposed as rows | `asset.state` and `history.inspect` |
| Export | Title bar | core | Editor follow-up | The export design |
| Locate | Notice on a missing original | core | Editor follow-up | [Source recovery](../specs/source-recovery.md) |
| Command palette (`Cmd+K`) | Overlay | core | Proposed | `schema.list` and `module.list` |
| Copy as JSON request, Show in schema | Control context menu | core | Proposed | The control's generated action |
| Pixel proof | Developer section, off by default | module | Implemented (M1) | `edit.set-pixel` |

A build shows only the modules its registry contains. Sections for later modules exist in the mockup to settle their place in the panel, not as placeholders in code; the desktop never lists a module that is not registered. The pixel proof tool is a test module: propose a Developer section that is hidden unless enabled by a launch flag, so the default workspace stays a photo editor.

## Panels

### State panel

- **Versions** are chips: name and entry number. The chip for the displayed entry is tinted. Selecting one previews that entry; a plus button saves the displayed state under a typed name. Delete is in the chip's context menu.
- **History** lists newest first: sequence number, a marker, a short label and the actor. The label is the action title plus a one-value summary when the module supplies one ("Shadows +25", "Crop 4:5", "Rotate right"). Markers: filled accent for the current entry, an accent outline for a previewed entry, hollow for the rest. Undone entries stay in the list, dimmed and labelled branch. Load older appears when a page remains.
- **Recipe** lists the layer stack in stored order with the module title and a compact payload summary. It is the durable order of processing, which is why it lives with history and not with the tools: the panel order on the right never changes it.
- During a historical preview the panel shows Return to current and Restore under the list and the tools panel is disabled with its values still visible, matching the current behaviour.

### Tools panel

- The **histogram** sits above the modules with no header. Its caption names the domain ("Output · sRGB · after crop") and the pointer readout shows the three output codes. The two triangles toggle the shadow and highlight overlays; a triangle is coloured when its endpoint has pixels and its tooltip carries the count and channels.
- Each **module section** has a header row: disclosure, title, an accent dot when the module has a non-neutral layer in the current recipe, and a reset action. Collapsed headers show a short hint. Unavailable modules show their reason in place of the hint and cannot expand. Section order follows the registry.
- **Sub-groups** inside a module (White balance, Tone, Color) come from the descriptor's `group` controls and carry their own reset.
- A **slider** is a label, a right-aligned tabular value that becomes a text field when clicked, and a 2 px track with a fill growing from the zero tick for bipolar ranges. The thumb turns accent while a draft is open. Invalid text stays editable with the declared range shown under the field and commits nothing. Option-click or double-click a label resets one field.
- **Transform** renders as one Crop and straighten button plus four icon buttons. Entering crop expands a Crop section at the top of the panel for the life of the draft and collapses it again on Apply or Cancel.

### Canvas

- At Fit the photograph is centred with 20 pt of surface around it; at a percentage it scrolls on both axes and Space-drag pans. 100% keeps its physical-pixel meaning.
- A **draft bar** appears at the top of the canvas while a mode has a draft: mode name, a one-line readout, Cancel (Escape) and Apply (Enter). The same values appear in the mode's section for keyboard editing.
- The crop overlay is the current one: dimmed outside, thirds, border and eight handles, with the crop layer's input stage shown underneath.
- **Notices** are cards at the top of the canvas: Changed elsewhere (Discard draft, Reapply), Original not found (Locate…), Preview is stale (the unavailable provider by name), and rendering limits. They never block the rest of the screen.
- Compare holds the Original entry's preview while `\` is down and releases it without touching history or session state beyond the preview.

## Interaction rules

- **Commit timing.** Adjustment sliders commit once on release, key-up or Enter; Escape cancels and focus loss cancels an unfinished gesture. Canvas modes commit only on Apply. Both follow the draft lifecycle from the [Basic and histogram design](basic-and-histogram.md) and the [crop contract](../specs/single-image.md).
- **One draft per client and asset.** Switching modes or expanding another module with a draft open asks for Apply or Cancel; it never discards silently.
- **Live agents.** An external commit updates history and the canvas immediately. If the client has a draft the draft is kept, the conflict notice appears and Apply is refused until Discard or Reapply, matching the accepted M4 behaviour. Every history row shows its actor, and the status bar counts connected clients.
- **Copy as JSON request** on any generated control writes the exact `edit.<action>` request for the control's current values, with the current expected revision, so a person can hand an agent what they just did.
- **Keyboard.** Existing: Cmd+O, Cmd+Z, Shift+Cmd+Z, Tab and Shift+Tab between fields, Enter and Escape in a draft, Space-drag. Proposed: `F` Fit, `1` 100%, `\` compare, `J` clipping, `O` thirds, `V` pointer, `R` crop, `W` neutral picker, arrows and Shift-arrows step a focused slider, Cmd+E export, Cmd+K command palette, Cmd+Option+[ and ] toggle the side panels. Letters act only when no text field has focus.

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

System UI face (SF Pro on macOS, the platform default elsewhere), 12 pt controls, 13 pt semibold titles, 11 pt captions, 10.5 pt capitalised section labels, tabular numerals everywhere a value can change width. 8 pt spacing grid, 6 pt radii, 1 px borders, no gradients or blur. Dark only for now; a light theme is a later decision, not a token swap in this proposal.

## What the desktop needs from the core

The screen is renderable from today's descriptors with a small set of additions, each visible through `module.list` or `session.state`: section hints, reset actions, history summary templates, per-layer summaries, canvas titles and shortcuts, the developer flag and per-client workspace state. They are specified in [core additions](#core-additions) below. Compare, the command palette and Copy as JSON request need no core change.

Widgets keep no authoritative state. Panel collapse, mode and overlay toggles are per-client session state, reported with `session.state` like zoom today. The tools panel refreshes only the section whose values changed, and slider drags produce one preview request per frame at most, per the [performance rules](../engineering/performance-rules.md).

## Acceptance

- A native M4 rendered check at 1440 × 900 and at the panels-collapsed size shows the five regions, the mode strip and the histogram with correlated state, revision and render generation.
- Every control in the tools panel is generated from `module.list` output; an independent JSON client can perform each control's action and see the same history entry, actor and label.
- Draft, conflict, historical preview and unavailable-provider states render the notices described above, with the existing M4 conflict tests extended to the Basic draft.
- Keyboard-only operation reaches every control; the status bar names the reason for every disabled action.
- The workspace opens without initialising any module resource, and expanding a section allocates nothing until its first action, measured separately as the module design requires.

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
| History labels | `ActionDescriptor.summary: Option<String>`, a template over the action's parameters (`{name}`). At commit the host renders it with the stored parameters (integers as written, numbers without trailing zeros, colours as `r,g,b`, enum options title-cased with hyphens as spaces) into `HistoryEntry.label`; without a template the label is the action title. The host labels Original and restore entries itself. Storing the label on entries moves the catalog to internal format 3; format 2 catalogs are refused explicitly |
| Recipe rows | `ToolModule::describe_layer` returns a short payload summary; the read-only method `recipe.describe {asset_id, entry_id?}` lists an entry's layers with module id, title, summary and availability. An unavailable provider's layers are listed with the reason instead of a summary |
| Mode strip | `CanvasInteraction` gains `title` and `shortcut` (one letter, unique across the registry, optional) on both variants |
| Developer section | `ModuleDescriptor.developer: bool`; the pixel module sets it. The desktop lists developer modules only with `--developer` |
| Workspace state | `ClientSession.workspace {state_panel, tools_panel, mode, thirds}` reported by `session.state` and set by `workspace.set` with optional fields; `mode` is `pointer` or an available module id that declares a canvas interaction |
| Evidence for an unavailable provider | `OwnerHandle::start_with` accepts a registry; the desktop's `--disable-module ID` flag registers that built-in wrapped as unavailable, so rendering a stack that uses it fails with the unavailable-effect error and the notice appears. `LocalServer::connected` reports live client count for the status bar |

Compare, the command palette and Copy as JSON request need no core change.

### What is tested where

| Layer | Tests |
| --- | --- |
| `lightwell-core` | Descriptor validation for reset, summary, shortcut and developer; label rendering; `recipe.describe`; `workspace.set` and `session.state`; format 3 refusal of a format 2 catalog; every addition through the JSON method table |
| `lightwell-ui` | Token values equal the visual language; the slider's fill geometry, value-field states and the mapping from a pointer position to a value are pure functions with tests; no test links `lightwell-core` |
| `lightwell-app/src/state/` | History labels and markers, branch marking, disabled reasons, conflict state, which section is expanded, what a slider shows while its value is being typed or dragged, the mode strip entries, the notices, the palette entries, per-section re-derivation |
| `lightwell-app/src/app/` | Every generated control's message produces the request an independent JSON client would send (parity), the keymap table, evidence steps, draft driving, compare restore |
| `xtask` | Repository boundary check; smoke scenarios `workspace` (default, each panel collapsed, historical preview, conflict during a draft), `crop`, `crop-draft` and `unavailable`; `measure` before and after |

