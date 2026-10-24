# Preferences

Status: implemented, on the [decisions](#decisions) below; verified on the M4 by the unit and owner tests, the `settings` smoke scenario, a check across background launches, and the `quick` and `rendered` tiers.

The person's preferences live outside every catalog, in the host's `preferences.json` beside the [feature flags](settings-and-flags.md). Beside the Performance disclosure and the General tab's Auto collapse history row, there are two kinds of preference:

- **Remembered state.** Choices the person makes in the workspace, kept across launches with no row of their own: panel visibility, the thirds and clipping overlays, the brush, the window's frame, the last export folder, the last HDR merge's settings and the Develop folder.
- **General rows.** Choices with no other home: whether new RAW photos get their lens profile, which look they start from, the mask overlay colour, the canvas background, the interface size and where the catalog lives.

## Rules

**Protected usage statistics record.** `preferences.json` also holds the [usage statistics](usage-statistics.md#consent-and-first-launch) consent record, `usage_statistics`. It is not a preference: only `usage.prompt.claim` and `usage.set` write it ([protected record](#protected-usage-statistics-record)). The durable prompt claim is bookkeeping, a deliberate exception to the explicit-choice rule below.

- **A preference never changes how a saved recipe renders.** It changes what the workspace draws, where files go, or which actions are proposed to a new photo. A proposed action is an ordinary history entry, so the recipe always records it.
- **Only explicit choices are stored.** A preference nobody set has no stored value and follows its default. `null` in `preferences.set` removes the stored value. A stored value this build cannot read is refused without rewriting the file.
- **Every preference is programmable.** `preferences.read` answers every preference with its default filled in, and `preferences.set` sets any of them, checked by name. The desktop reads and writes them through the same two methods.
- **Automated launches never touch the person's preferences.** An evidence run keeps its preferences inside its evidence directory, and a hidden launch never writes the window's frame.
- **Nothing moves the person's files.** Choosing a catalog location opens or creates a catalog at the next launch. It never moves, copies or deletes the open catalog.

## Preferences

| Field | Value | Default | Shown | Takes effect |
| --- | --- | --- | --- | --- |
| `performance_expanded` | boolean | `true` | Performance disclosure | at once |
| `auto_collapse_history` | boolean | `true` | General: **Auto collapse history** | next edit |
| `auto_lens_profile` | boolean | `true` | General: **Correct lens distortion on new RAW photos** | next first preparation |
| `raw_look` | `standard` or `neutral` | `standard` | General: **Starting look for new RAW photos** | photographs created from then on |
| `mask_overlay_colour` | `green` or `white` | `green` | General: **Mask overlay colour**, and the Masks panel's colour control | at once |
| `canvas_background` | `theme`, `dark`, `black` or `grey` | `theme` | General: **Canvas background** | at once |
| `theme` | a theme id, or `null` | `null`: Luxforge Dark | Settings › Appearance ([UI themes](ui-themes.md)) | at once |
| `theme_follows_omarchy` | boolean | `true` | Settings › Appearance: **Follow Omarchy's theme**, where Omarchy runs ([following Omarchy](ui-themes.md#following-omarchy)) | at once |
| `interface_size` | `100`, `110`, `125` or `150` | `100` | General: **Interface size** | at once |
| `catalog` | absolute path of a catalog file, or `null` | `null`: `catalog.sqlite` in the configuration directory | General: **Catalog** | next launch |
| `workspace` | `{state_panel, tools_panel, thirds, clip_shadows, clip_highlights}` | the workspace's defaults | remembered | next launch |
| `brush` | `{size, feather, flow}`, or `null` | `null`: the neutral brush | remembered | next launch |
| `window` | `{width, height, x, y}` in the system's points, or `null` | `null`: 1440 × 900, placed by the system | remembered | next launch |
| `export_last` | `{settings, preset_id}`, or `null` | `null`: the sheet opens on Web | remembered | next export |
| `hdr_merge` | `{alignment, deghost}`, each `auto` or `off`, or `null` | `null`: both Auto | remembered | next ⌥-click merge |
| `panorama_merge` | `{projection, boundary_warp}`: `auto`, `perspective`, `cylindrical` or `spherical`, and 0–100; or `null` | `null`: Auto and 0 | remembered | next ⌥-click merge |
| `select_introduction_seen` | boolean, or `null` | `null`: not seen | remembered: closing the [Select introduction](select-introduction.md) sets it | next launch |
| `develop_folder` | `{catalog_id, folder_id}`: `catalog.info`'s identity of its catalog, 1 to 64 printable characters, and a catalog folder identity; or `null` | `null`: nothing chosen | remembered: the [Develop folder](develop-from-folders.md#develops-folder) | next launch over that catalog |

### Protected usage statistics record

`usage_statistics` is `{prompt_shown, enabled, disclosure_version, installation_id?, image_key?}`. An absent record means the notice was never shown and sharing is off. An enabled record holds a random UUIDv4 `installation_id` and a 64-hex-character `image_key` (32 random bytes); a disabled one holds neither, and any other combination, a non-canonical ID or key, an unknown key or a disclosure version of 0 makes the file unreadable, so it is refused unchanged as any other bad value is and statistics stay off.

- `preferences.set` has no `usage_statistics` parameter, so naming it, `null` or not, is refused, and no value or reset of another preference changes the record. `flags.set` keeps it too.
- `preferences.read` and `preferences.set` answer it as `{prompt_shown, enabled, disclosure_version?, installation_id?}`, `{prompt_shown: false, enabled: false}` when absent, and never with its `image_key`.
- `usage.prompt.claim` and `usage.set` each make one locked read-modify-write of the record alone, leaving every preference as it was. Their rules, and `usage.read`, are in the [usage statistics design](usage-statistics.md#settings-and-programmatic-parity).
- The desktop's statistics supervisor, started in an eligible launch, rereads the record from the same configuration directory off the owner before detaching a window and before each upload, and never writes it ([implemented integration](usage-statistics.md#implemented-integration)). An evidence launch given `--evidence-config` keeps an earlier evidence run's configuration, so a choice one launch stored is the one the next reads.

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
- `raw_look`
- `mask_overlay_colour`
- `canvas_background`
- `interface_size`
- `catalog`

and whenever `theme` or `theme_follows_omarchy` changes.

An open Settings sheet reads them again. The desktop applies the theme, the canvas background, the interface size and the mask overlay colour from what it reads, so an agent's change shows at once. Remembered state is the desktop's own bookkeeping and announces nothing.

## Behaviour

- **Correct lens distortion on new RAW photos.** On by default: the [first-open lens action](lens-and-perspective.md) is committed. Off, a photograph's first preparation does not ask the lens module for a first-open action. The Lens section offers the detected profile with Apply, as it does for any photo without one. The change applies to first preparations from then on; it never adds or removes an entry on a photograph whose head has moved. A photograph still at its Original (revision 0) is asked at its next preparation, so turning the switch on later can still correct it there. This is a core preference, so every client follows it.
- **Starting look for new RAW photos.** Standard by default: a new RAW photograph's Original holds Luxforge's look beside its development ([RAW looks](raw-looks.md#starting-a-new-photo)). Neutral, it holds the bare development. The host asks every module for its contribution to a new photograph's Original (`ToolModule::original`) and hands each the look, so the change applies to photographs created from then on, by a Develop or in a seeded catalog; it never rewrites a photograph already in the catalog or a saved recipe. JPEG photographs get no look. This is a core preference, so every client follows it.
- **Mask overlay colour.** The colour the Tint overlay is drawn in. The General row and the Masks panel's control both set this desktop's session colour (`workspace.set {mask_overlay_colour}`) and store the preference.
- **Canvas background.** The colour around the photograph:
  - `theme`, the active theme's surround, held neutral ([UI themes](ui-themes.md#rules)); for Luxforge Dark the same `#19191b` as `dark`;
  - `dark`, `#19191b`;
  - `black`, `#000000`;
  - `grey`, an 18% grey (L\* 50, `#777777`), for judging tone the way a print is judged.

  It fills the canvas region, the photo surface outside the photograph and both sides of the compare canvas, which draws no fill of its own. The panels, the histogram and readout, the title bar, the compare canvas's Before and After labels and the mask overlay's on-black modes keep their own colours.
- **Interface size.** It scales everything Iced draws, through the application's scale factor.
  - **Pixels.** The view state's scale factor is the system's factor multiplied by the interface size, so every physical-pixel computation, and 100% zoom, stays one source pixel per display pixel. Values that arrive in the system's points, such as the system's own factor and the trackpad's pinch position, are converted where they arrive.
  - **Changing it.** Iced keeps the window's physical size and sends no resize, so the desktop rescales its logical window size itself and Fit, the clipping grid and region requests follow through the ordinary view-geometry path. The window never resizes.
  - **The rest.** The Settings sheet shrinks to fit a window too small for it. The status bar's `2×` names the display's own factor. A window moved to a display of another scale is not followed.
- **Catalog.** The row shows the catalog this launch uses, with **Choose Folder…**, and **Use Default** when a location is stored.
  - **Choosing.** Choose Folder… opens a native folder dialog in the open catalog's folder and stores `<folder>/catalog.sqlite`. At the next launch the desktop opens that catalog, creating it in the current catalog format if the folder holds none, as it creates the default; its `catalog.index` cache and any `catalog.artifacts` live beside it in that folder. The open catalog stays where it is, and the row says so.
  - **Notes.** While the stored location differs from this launch's, the row says "Relaunch to use `<path>`". `--catalog` and evidence runs take precedence, and the row says "This launch uses `--catalog`" or "This launch uses the evidence run's catalog".
  - **Missing folder.** If the stored catalog's folder does not exist at launch, as with an unplugged drive, the desktop opens the default catalog instead. The status bar says "Catalog folder not found: `<folder>`; using the default catalog", and the row repeats it. The stored location is kept, so the next launch with the drive present opens it. The event log records `catalog_folder_missing`.
  - **Other failures.** A catalog that cannot be opened for any other reason (another instance owns it, its format is unsupported, or checking its parent reports an access error or finds a regular file) is refused as the default catalog is: the launch stops, naming the catalog and, for an older catalog, its format and the one expected, and the file is left unchanged. Only a missing parent selects the default; other parent-check failures keep the stored path for the opening error. There is no fallback to the default for these, so the way past an older catalog is a launch with `--catalog` and Use Default there.
  - **The command line.** `luxforge-json` requires `--catalog`.
- **Remembered workspace.** As the editor is built, before its first frame, the desktop starts its session from the stored `workspace` and `mask_overlay_colour` through one `workspace.set`, sent only when they differ from the defaults, so a launch with nothing stored sends nothing.
  - **Storing.** After the desktop adopts a `workspace.set` answer that changed any of the five remembered fields, it stores them. This covers the panel toggles, `O`, `J`, the histogram's triangles, the palette and an evidence run's own workspace steps, which store into the evidence directory.
  - **Not remembered.** The canvas mode, the mask overlay mode and zoom.
  - **Unreadable preferences.** When the preferences could not be read at launch, nothing remembered is stored that session, so the status bar is not filled with refusals.
- **Remembered brush.** The Masks panel's brush starts from the stored `brush`, or the neutral brush. A change of size, feather or flow by key, nudge, slider, typed value or reset is stored; a reset stores the neutral numbers. Erase, Limit to colour and the colour refine belong to the stroke and are not remembered.
- **Remembered window.**
  - **At close.** The desktop asks Iced once for the window's size, position, display and mode, and stores the frame, converting Iced's logical size back to the system's points by the interface size. A close in fullscreen, or with no position reported, keeps the frame stored before, and an unchanged frame is not written again.
  - **At launch.** The window opens at the stored size, which the launch divides by the interface size for Iced, at the stored position. `--window-size` ignores the stored frame and lets the system place the window, though that launch's close still stores its frame. A hidden or evidence launch neither opens at a frame nor stores one.
  - **Off-display check.** Iced reports a display's size but not where it sits among the others. After opening, a frame no display holds is moved to the main display and checked again. A frame on the main display, taken to be the one whose bounds hold its top-left corner, that does not lie wholly within it is centred there at its size, shrunk to fit. A frame on another display is kept if it fits that display's size, and otherwise moved to the main one.
  - **Limit.** A secondary display larger than the main one, to its right or below it, can be taken for the main one, so a frame running past its edge can be moved to the main display. Exact placement needs the native screen frames.
  - **Events.** The event log records `window_fitted` and `window_moved_to_main_display`.
- **Remembered export.** After an export the person started from the Export sheet, the Export menu or the palette succeeds, the desktop stores its settings and the preset they came from unchanged, or `null`. The sheet opens on them and Export again repeats them ([export settings](export-settings.md#the-export-sheet)). An API client's export stores nothing.
- **Remembered HDR merge.** When a merge this desktop asked for starts, the desktop stores its alignment and deghosting as `hdr_merge`, if they differ from what is stored. ⌥-click on Merge HDR merges with them, and the [merge preview](panorama-merge.md#the-merge-preview) opens with them; with nothing stored, both are Auto. The folder, the reference and exposure overrides belong to one bracket and are not remembered.
- **Remembered panorama merge.** Likewise, a panorama merge this desktop asked for stores its projection and Boundary Warp as `panorama_merge`; ⌥-click on Merge panorama and a newly opened panorama preview start from them, else from Auto and 0. The reference and the folder are not remembered.
- **Remembered Develop folder.** The catalog folder Develop holds, stored with the identity of its catalog whenever it changes, and `null` when a deletion clears it. A launch with no file to open over the same catalog views it again in Select while the catalog still lists it ([across launches](develop-from-folders.md#develops-folder)). One folder is remembered, the last chosen in any catalog: a launch over another catalog neither views nor clears it until a folder is chosen there. It belongs to the catalog it names, so it is keyed by that catalog's identity rather than its path, and it lives here, beside the rest of the desktop's remembered state, rather than in the catalog, which holds no client's view state.

## Desktop writes

All of the desktop's `preferences.set` calls go through one writer:

- **One at a time.** One call is in flight. A later change waits, merged with any change already waiting, with the newer value of a field replacing the older.
- **Shown at once.** A change shows immediately, and the answer that lands confirms it or puts the stored value back.
- **Refusals.** A refused write says "Could not save preferences: `<reason>`" in the status bar, logs `preference_set_failed` with its fields, and reads back what is stored. A failed mask overlay colour save also reconciles this session's colour with that read, even when the stored preference stayed unchanged; a newer waiting colour choice keeps precedence.
- **Closing.** Closing the window waits for the last write.

A held `[` key that resizes the brush therefore writes at most one call in flight plus one waiting, whatever the key repeat rate.

## Evidence and verification

- **Core unit tests.** Each field round-trips. `null` resets. Every bad value is refused by name. A malformed file is refused without being rewritten. Only the General fields announce.
- **First open.** First open skips the lens action when `auto_lens_profile` is off and still commits it when on.
- **New photographs.** The owner hands `raw_look` to the Original hook at start and after `preferences.set`, for each photograph a `pick.develop` creates from then on. A test module's layer lands in each new RAW Original where its stage places it, a JPEG's Original is untouched, a refused or failing contribution refuses its pick by the module's name, and a seeded RAW follows the same rule. The built-in Look module contributes the Standard RAW look; Neutral contributes none. The look module and owner tests prove both paths.
- **Export.** `export.plan` suggests the remembered folder only while it exists.
- **Launch.** Resolution reads the catalog and window, `--catalog` and `--window-size` override them, and a missing catalog folder falls back to the default with the reason.
- **Desktop unit tests.**
  - The writer merges changes, coalesces them, refuses where it must and waits at close.
  - Every General row's model is checked.
  - Remembered state is seeded and stored.
  - A frame records the combined scale factor at each interface size, and 100% maps one source pixel to one physical pixel at 125%.
  - The canvas colour is checked for each choice.
- **Evidence steps.** `{"preference": {<field>: <value>}}` drives the General rows' own messages, sending only values the rows do not already show and waiting for the writer to go idle. The Catalog row takes the catalog file's path, as its dialog's answer, or `null` for Use Default.
- **Frame state.** Each frame's state records `preferences`: `display` (canvas background, interface size, and the system's and combined scale factor), `applied`, `stored`, `writing`, `waiting` and `error`.
- **The `settings` smoke scenario** runs its own launch, so its catalog step can name a folder inside the run's evidence directory, and covers:
  - the canvas background set to grey, with the canvas pixels beside the photograph exactly `#777777`, and no pixel of the panels or the photograph changed, then back to dark;
  - the interface size set to 125%, with the combined scale factor recorded and the title bar's drawn height 1.25 times its height at 100%, then back to 100%;
  - the mask overlay colour set to white;
  - the lens switch turned off, checked on the row and in what is applied and stored. No RAW is checked into the repository, so developing one with the switch off is proven by the owner tests instead;
  - a catalog folder chosen inside the evidence directory, with the row's relaunch note checked.
- **Across launches.** The integrator launches `cargo xtask develop --background --hidden-window` over one isolated `--data-root`. One launch stores a catalog location through `preferences.set`; the next opens it, shown by where its live-session file appears; a location whose folder is missing opens the default catalog and logs `catalog_folder_missing`. The desktop's own session is not readable by another client, and a hidden launch never opens at or stores a window frame, so the remembered workspace, brush and window are proven by the unit tests that build the editor over stored preferences.

## Performance rules checklist

- **Original reads and decodes.** None added.
- **Full-frame allocations.** None. `preferences.json` stays bounded at 16 KiB, and the two paths are its largest values.
- **Point queries.** None added.
- **Owner thread.** `preferences.read` reads one small file. Each `preferences.set` is one locked read-modify-write of it. A first preparation with the lens switch off does less work. A new photograph's Original asks each module once, from metadata, with no file or pixel read.
- **Desktop messages.**
  - Each remembered change adds at most one `preferences.set`, coalesced by the writer. The window's frame is written once, at close.
  - A changed canvas background or interface size draws one new frame. The interface size reflows the layout once.
  - No `asset.state`, `history.list`, preview job or upload is added. The photograph is not rendered again: a new interface size changes the photo surface's physical size, and the ordinary resize path handles that.
- **Timers, polls and subscriptions.** None added.
- **Repeated work and caches.** None added.
- **Timing.** No interactive path changes cost, so no `editor-performance` run is needed.
- **Exactness.** No preference changes a rendered or exported byte. A lens entry that is not proposed is simply absent from history.

## Decisions

- Remember panel visibility, the thirds and clipping overlays, the brush, the window's frame and the last export folder across launches, with no Settings rows of their own.
- Add the mask overlay colour, the canvas background and a switch for automatic lens correction to General.
- Add an interface size and a catalog location to General.
- **Export defaults:** the [export settings](export-settings.md) hold quality, metadata and the folder in export presets, and the last export is remembered as `export_last`, with no Settings row.

Recorded defaults, proposals the owner can revise:

- The field names and shapes above.
- **Canvas background:** four choices, Theme the default, with grey at L\* 50.
- **Interface size:** four sizes, 100, 110, 125 and 150%.
- **Catalog:** a folder holding `catalog.sqlite`, applied at the next launch, falling back to the default catalog when its folder is missing.
- **Window:** the frame is not remembered in fullscreen.
- **Brush:** only size, feather and flow are remembered.
- **Export folder:** stored only after an export the person chose in the dialog.
- **Develop folder:** one folder remembered, for the catalog last used, keyed by its `catalog_id`; viewed again only by a launch with no file to open, and only in Select, so a launch that starts in Develop opens no photograph.
- **Lens switch:** on by default, applying only to photographs still at their Original when first prepared.
