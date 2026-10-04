# Preferences

Status: in implementation, at the owner's request of 2026-10-04 and on the [decisions](#decisions) below. Plan: [tasks/preferences.json](../../tasks/preferences.json).

The person's preferences live outside every catalog, in the host's `preferences.json` beside the [feature flags](settings-and-flags.md). This design adds two kinds of preference to the one Auto collapse history row the Settings sheet's **General** tab has today:

- **Remembered state.** Choices the person already makes in the workspace, kept across launches with no row of their own: panel visibility, the thirds and clipping overlays, the brush, the window's frame and the last export folder.
- **General rows.** Choices with no other home: whether new RAW photos get their lens profile, the mask overlay colour, the canvas background, the interface size and where the catalog lives.

## Rules

- **A preference never changes how a saved recipe renders.** It changes what the workspace draws, where files go, or which actions are proposed to a new photo. A proposed action is an ordinary history entry, so the recipe always records it.
- **Only explicit choices are stored.** A preference nobody set has no stored value and follows its default. `null` in `preferences.set` removes the stored value. A stored value this build cannot read is refused, as today, without rewriting the file.
- **Every preference is programmable.** `preferences.read` answers every preference with its default filled in, and `preferences.set` sets any of them, checked by name. The desktop reads and writes them through the same two methods.
- **Automated launches never touch the person's preferences.** An evidence run keeps its preferences inside its evidence directory, and a hidden launch never writes the window's frame.
- **Nothing moves the person's files.** Choosing a catalog location opens or creates a catalog at the next launch. It never moves, copies or deletes the open catalog.

## Preferences

| Field | Value | Default | Shown | Takes effect |
| --- | --- | --- | --- | --- |
| `performance_expanded` | boolean | `true` | Performance disclosure (today) | at once |
| `auto_collapse_history` | boolean | `true` | General: **Auto collapse history** (today) | next edit |
| `auto_lens_profile` | boolean | `true` | General: **Correct lens distortion on new RAW photos** | next import |
| `mask_overlay_colour` | `green` or `white` | `green` | General: **Mask overlay colour**, and the Masks panel's colour control | at once |
| `canvas_background` | `dark`, `black` or `grey` | `dark` | General: **Canvas background** | at once |
| `interface_size` | `100`, `110`, `125` or `150` | `100` | General: **Interface size** | at once |
| `catalog` | absolute path of a catalog file, or `null` | `null`: `catalog.sqlite` in the configuration directory | General: **Catalog** | next launch |
| `workspace` | `{state_panel, tools_panel, thirds, clip_shadows, clip_highlights}` | the workspace's defaults | remembered | next launch |
| `brush` | `{size, feather, flow}`, or `null` | `null`: the neutral brush | remembered | next launch |
| `window` | `{width, height, x, y}` in the system's points, or `null` | `null`: 1440 × 900, placed by the system | remembered | next launch |
| `export_folder` | absolute path of a folder, or `null` | `null`: the original's folder | remembered | next export |

### Checks

`preferences.set` refuses a bad value by name:

- a value outside its options;
- a brush number outside the range `mask.add-stroke` declares for it;
- a window width or height outside 320 to 16,384, or a position that is not finite;
- a relative path or an empty one.

A path is not required to exist, since a drive can be unplugged. Its use checks it.

### Events

`preferences.set` announces in the event log only when it changes something a General row shows:

- `auto_collapse_history`
- `auto_lens_profile`
- `mask_overlay_colour`
- `canvas_background`
- `interface_size`
- `catalog`

An open Settings sheet reads them again. The desktop applies the canvas background, the interface size and the mask overlay colour from what it reads, so an agent's change shows at once. Remembered state is the desktop's own bookkeeping and announces nothing.

## Behaviour

- **Correct lens distortion on new RAW photos.** On by default: the [first-open lens action](lens-and-perspective.md) is committed as it is today. Off, an import does not ask the lens module for a first-open action. The Lens section offers the detected profile with Apply, as it does for any photo without one. The change applies to imports from then on; it never adds or removes an entry on a photo already in the catalog. This is a core preference, so every client's imports follow it.
- **Mask overlay colour.** The colour the Tint overlay is drawn in. The General row and the Masks panel's control both set this desktop's session colour (`workspace.set {mask_overlay_colour}`) and store the preference.
- **Canvas background.** The colour around the photograph:
  - `dark`, today's `#19191b`;
  - `black`, `#000000`;
  - `grey`, an 18% grey (L\* 50, `#777777`), for judging tone the way a print is judged.

  It fills the canvas region, the photo surface outside the photograph and the compare canvas. The panels, the title bar and the mask overlay's on-black modes keep their own colours.
- **Interface size.** It scales everything Iced draws, through the application's scale factor. Every physical-pixel computation uses the system's factor multiplied by it, so 100% zoom stays one source pixel per display pixel. The Settings sheet shrinks to fit a window too small for it at the chosen size. The window's remembered frame stays in the system's points, so changing the size never resizes the window.
- **Catalog.** The row shows the catalog this launch uses, with **Choose Folder…**, and **Use Default** when a location is stored.
  - **Choosing.** Choose Folder… stores `<folder>/catalog.sqlite`. At the next launch the desktop opens that catalog, creating it if the folder holds none, as it creates the default today. The open catalog stays where it is, and the row says so.
  - **Notes.** While the stored location differs from this launch's, the row says "Relaunch to use `<path>`". `--catalog` and evidence runs take precedence, and the row says "This launch uses `--catalog`".
  - **Missing folder.** If the stored catalog's folder does not exist at launch, as with an unplugged drive, the desktop opens the default catalog instead. The status bar says "Catalog folder not found: `<folder>`; using the default catalog", and the row repeats it. The stored location is kept, so the next launch with the drive present opens it.
  - **Other failures.** A catalog that cannot be opened for any other reason (another instance owns it, or its format is unsupported) is refused as the default catalog is today.
  - **The command line.** `luxforge-json` keeps requiring `--catalog`.
- **Remembered workspace.** At launch the desktop starts its session from the stored `workspace` and `mask_overlay_colour` through one `workspace.set`. After it adopts a `workspace.set` answer that changed any of the five remembered fields, it stores them. The canvas mode, the mask overlay mode, zoom and the GPU preview are not remembered.
- **Remembered brush.** The Masks panel's brush starts from the stored `brush`, or the neutral brush. A change of size, feather or flow by key, nudge, typed value or reset is stored. Erase, Limit to colour and the colour refine belong to the stroke and are not remembered.
- **Remembered window.** At close the desktop stores the window's frame, converting Iced's logical size back to the system's points by the interface size. A close in fullscreen keeps the frame stored before. At launch, `--window-size` overrides the stored size. If the stored frame does not lie within the display the window opens on, the window opens centred there at the stored size, shrunk to fit.
- **Remembered export folder.** After an export whose destination the person chose in the save dialog succeeds, the desktop stores the destination's folder. When `export_folder` is stored and is an existing folder, `export.plan` suggests `<export_folder>/<stem>-edited.jpg`, counting up within that folder. Otherwise it suggests the original's folder as today. An export with a destination given by an evidence step or an API client stores nothing.

## Desktop writes

All of the desktop's `preferences.set` calls go through one writer:

- **One at a time.** One call is in flight. A later change waits, merged with any change already waiting, with the newer value of a field replacing the older.
- **Shown at once.** A change shows immediately, and the answer that lands confirms it or puts the stored value back.
- **Refusals.** A refused write says why in the status bar.
- **Closing.** Closing the window waits for the last write, as the Performance preference does today.
- **Existing writes.** The Performance disclosure and Auto collapse history writes move onto this writer.

A held `[` key that resizes the brush therefore writes at most one call in flight plus one waiting, whatever the key repeat rate.

## Evidence and verification

- **Core unit tests.** Each field round-trips. `null` resets. Every bad value is refused by name. A malformed file is refused without being rewritten. Only the General fields announce.
- **First open.** First open skips the lens action when `auto_lens_profile` is off and still commits it when on.
- **Export.** `export.plan` suggests the remembered folder only while it exists.
- **Launch.** Resolution reads the catalog and window, `--catalog` and `--window-size` override them, and a missing catalog folder falls back to the default with the reason.
- **Desktop unit tests.**
  - The writer merges changes, coalesces them, refuses where it must and waits at close.
  - Every General row's model is checked.
  - Remembered state is seeded and stored.
  - A frame records the combined scale factor at each interface size, and 100% maps one source pixel to one physical pixel at 125%.
  - The canvas colour is checked for each choice.
- **Evidence steps.** `{"preference": {<field>: <value>}}` drives the General rows' own messages. Each frame's state records the preferences the desktop applies.
- **The `settings` smoke scenario** gains:
  - the canvas background set to grey, with the canvas pixel checked beside the photograph;
  - the interface size set to 125%, with the title bar's drawn height checked against 100%;
  - the mask overlay colour set to white;
  - the lens switch turned off before importing a RAW, checking that no lens entry is committed;
  - a catalog folder chosen inside the evidence directory, with the row's relaunch note checked.
- **Across launches.** The integrator relaunches `cargo xtask develop --background` twice over one isolated `--data-root`, and checks that the panels, overlays, brush, window frame and catalog location carry over.

## Performance rules checklist

- **Original reads and decodes.** None added.
- **Full-frame allocations.** None. `preferences.json` stays bounded at 16 KiB, and the two paths are its largest values.
- **Point queries.** None added.
- **Owner thread.** `preferences.read` reads one small file. Each `preferences.set` is one locked read-modify-write of it. An import with the lens switch off does less work.
- **Desktop messages.**
  - Each remembered change adds at most one `preferences.set`, coalesced by the writer. The window's frame is written once, at close.
  - A changed canvas background or interface size draws one new frame. The interface size reflows the layout once.
  - No `asset.state`, `history.list`, preview job or upload is added. The photograph is not rendered again: a new interface size changes the photo surface's physical size, and the existing resize path handles that.
- **Timers, polls and subscriptions.** None added.
- **Repeated work and caches.** None added.
- **Timing.** No interactive path changes cost, so no `editor-performance` run is needed.
- **Exactness.** No preference changes a rendered or exported byte. A lens entry that is not proposed is simply absent from history.

## Decisions

Decided by the owner on 2026-10-04:

- Remember panel visibility, the thirds and clipping overlays, the brush, the window's frame and the last export folder across launches, with no Settings rows of their own.
- Add the mask overlay colour, the canvas background and a switch for automatic lens correction to General.
- Add an interface size and a catalog location to General.
- The GPU preview stays a per-session switch, not a preference.
- Export defaults (JPEG quality, Keep metadata) wait for the export work.

Recorded defaults, proposals the owner can revise:

- The field names and shapes above.
- **Canvas background:** three choices, with grey at L\* 50.
- **Interface size:** four sizes, 100, 110, 125 and 150%.
- **Catalog:** a folder holding `catalog.sqlite`, applied at the next launch, falling back to the default catalog when its folder is missing.
- **Window:** the frame is not remembered in fullscreen.
- **Brush:** only size, feather and flow are remembered.
- **Export folder:** stored only after an export the person chose in the dialog.
- **Lens switch:** on by default, applying to new imports only.
