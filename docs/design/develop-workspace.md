# Develop workspace

Status: proposal. This is the design for the single-image editing screen: what it shows, how the tools are arranged and how they behave. Library, catalog management and settings are out of scope. Nothing here is implemented by this document, and the tool list below marks what exists today against what is only planned. Product choices stay proposals until the owner decides; see [open decisions](#open-decisions).

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

The screen is renderable from today's descriptors with these additions, each small and each also visible through `module.list`:

| Need | Proposed descriptor or API change |
| --- | --- |
| Section hints, reset actions, sub-group resets | A `reset` preset on `group` and module level, and an optional `hint` string on the descriptor |
| History labels such as "Shadows +25" | Actions may declare a `summary` template over their parameters; the host stores the rendered label with the entry |
| Recipe rows | A payload summary string returned by the module for each of its layers |
| Mode strip | A module's `canvas` declaration already names the interaction; add a `title` and a `shortcut` so the strip can label it |
| Developer section | A launch flag that lists test modules; the registry marks them `developer: true` |
| Copy as JSON request | No core change; the desktop already holds the action id, parameters and expected revision |
| Command palette | No core change; `schema.list` and `module.list` supply the entries |

Widgets keep no authoritative state. Panel collapse, mode, overlay toggles and compare are per-client session state, reported with `session.state` like zoom today. The tools panel refreshes only the section whose values changed, and slider drags produce one preview request per frame at most, per the [performance rules](../engineering/performance-rules.md).

## Acceptance

- A native M4 rendered check at 1440 × 900 and at the panels-collapsed size shows the five regions, the mode strip and the histogram with correlated state, revision and render generation.
- Every control in the tools panel is generated from `module.list` output; an independent JSON client can perform each control's action and see the same history entry, actor and label.
- Draft, conflict, historical preview and unavailable-provider states render the notices described above, with the existing M4 conflict tests extended to the Basic draft.
- Keyboard-only operation reaches every control; the status bar names the reason for every disabled action.
- The workspace opens without initialising any module resource, and expanding a section allocates nothing until its first action, measured separately as the module design requires.

## Open decisions

| Decision | Recommendation | Effect if changed |
| --- | --- | --- |
| Panel split | State left, tools right, both collapsible | A single right panel keeps history further from the photo and mixes state with tools |
| Module presentation | Stacked collapsible sections in registry order | A one-tool-at-a-time icon rail is cleaner but hides the recipe's breadth and breaks Lightroom familiarity |
| History label | Action title plus one-value summary from the module | Raw action ids are exact but read as logs, not edits |
| Test modules | Hidden Developer section behind a launch flag | Showing pixel proof by default keeps the panel honest but not a photo editor |
| Compare | Hold `\` for the Original entry | A side-by-side view needs a second render and a different layout |
| Light theme | Not now | A second theme doubles the rendered-evidence surface |

Planning authorization is not implementation authorization. The next step after the owner's review is a task plan for the shell, the state panel and the mode strip against the current modules, with the Basic and histogram work landing in the sections this design reserves for them.
