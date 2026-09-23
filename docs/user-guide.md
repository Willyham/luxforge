# Lightwell user guide

The Develop workspace opens JPEG and the camera recording modes listed in the bundled RAW catalog, with exact transforms, crop/straighten, persistent history and the JSON API. RAW adds editable source exposure and white balance. Export, Locate and MCP are planned; see [feature status](features.md).

JPEG Basic exposure, tone, white balance and colour controls are built, with the neutral picker, and so are the histogram and clipping inspector that share their [design](design/basic-and-histogram.md), and the Presence, Colour mixer and Vignette sections of their own [design](design/presence-mixer-vignette.md). [Presets](design/presets.md) save and apply those settings, and import Lightroom Classic presets.

RAW profiles cover selected modes from Nikon, Canon, Sony, Fujifilm, Panasonic, Olympus/OM System, Pentax, Ricoh, Leica and DJI. The [camera selection](../fixtures/modern-camera-selection.json) covers 100 enabled models, including high-resolution bodies; a listed camera does not imply every compression, resolution, burst or drone camera-module mode is supported. The original Z6 lossless NEF, X100VI uncompressed/lossless RAF and Air 2S FC3411 DNG modes remain covered. DNG development applies the supported embedded corrections in their recorded order. Unknown required corrections fail explicitly. Broader recording modes and controlled color/detail qualification remain open. A failed Open keeps the previous photo. The bundled [camera catalog](../crates/lightwell-raw/data/cameras.json) defines admission; a matching filename extension alone does not enable a camera.

## Start the editor

After [developer setup](engineering/development.md), start an optimized build with a catalog and an optional original:

```sh
cargo xtask develop --catalog /path/to/catalog.sqlite --open /path/to/photo.jpg
```

Omit `--catalog` to use the platform configuration directory. `--data-root DIRECTORY` isolates config, cache and log paths. `cargo xtask develop --debug` is an unoptimized build for debugging and is unsuitable for timing.

`--developer` shows the pixel and controls proof modules under Developer. It also registers `lightwell.controls` and its API methods; without the flag that module is absent from both discovery and the workspace. The pixel proof remains discoverable through the API either way. `--disable-module lightwell.crop` (or another built-in id) registers that module as unavailable, which keeps its stored layers readable and reports them instead of rendering without them.

For agent-driven API or rendered checks on macOS, add `--background` to keep the editor from taking desktop focus. Use a separate test catalog or `--evidence-dir NEW_DIR`; background evidence runs capture the editor and exit automatically. `--hidden-window` additionally creates the window invisible: it still renders and captures through a real surface, but never appears on the desktop. Smoke and diagnostic harnesses use background, hidden launches by default on macOS. Launch normally for keyboard, mouse and native-dialog interaction.

Open references an existing supported original without copying or modifying it. JPEG input supports the existing sRGB/greyscale subset; RAW input is developed from sensor data. Cmd+O on macOS and Ctrl+O elsewhere opens the picker. EXIF orientation is applied once before any edit. The catalog stores stable identities, the source fingerprint, ordered operations and history. It is not a backup of the original photo.

Choose Copy beside the status text at the bottom of the window to copy the complete message, including any error, to the clipboard. The message stays visible after copying. The rest of the status bar shows the pointer readout while the pointer is over the photograph (see [Histogram and clipping](#histogram-and-clipping)), then how many clients the live API has, what the renderer is doing or how long the picture on screen took to render, and the current zoom with what it means on this display. The render time is the renderer's own time for that picture. When the photograph is larger than the space Fit shows it in, the picture at Fit is a render at the display's size and the figure says so — "Rendered in 12 ms (proxy)"; at 100% it is the full-resolution render. Zooming between the two shows the time of whichever picture is now on screen.

## The workspace

The window has five regions: a title bar with the file name, Open, the view control, Compare, Undo, Redo and two panel toggles; the state panel on the left (versions, history, recipe); the photograph in the middle with a floating mode strip under it; the tools panel on the right; and a status bar. Cmd+Option+[ and Cmd+Option+] hide and show the side panels, and the canvas takes whatever remains. Panel visibility, the canvas mode and the thirds overlay are session state, reported by `session.state` and settable through `workspace.set` like zoom.

Cmd+K opens the command palette: type to filter every module action, reset and canvas mode plus Fit, 100%, Undo, Redo, Return to current, Restore, Pointer, Thirds and the panel toggles, then Enter or click runs the entry through the same path the control uses. Right-click any generated control and choose Copy as JSON request to put the exact `edit.<action>` request for its current values, with the current expected revision, on the clipboard; the crop draft's Apply offers its `edit.crop` request the same way.

## Edit and inspect

The tools panel is generated from registered modules. Each module is a band you click to open or close, with its hint while closed and its reset while open; groups inside it are separated by a rule that ends in their Original or Custom caption and their reset. A section whose controls form a single group, such as RAW, Presence, Transforms or Vignette, shows them directly under its band with no group rule. Only Basic starts open, so every section's band fits on screen under it. Declared resets are offered, and an unavailable section shows its reason on its band. Every control reflects the displayed history entry, including edits made by another API client. Historical previews keep the controls visible and disabled.

Numbers use a slider, a field, or a stepper. A slider's rail may have a narrower soft range than typing allows; an over-range mark keeps values outside the rail visible. Click a value to type and press Enter to commit; leaving the field commits nothing, and invalid text remains editable with its range message. Drag values use the declared step and display precision. Double-click a label or slider rail to reset it. Arrows step, Shift steps ×10 and Option uses the fine step (with extra decimals when needed to show it); Iced sliders receive arrows only while the pointer is over the rail. Scrolling the panel never changes a value.

Toggles commit on click or Space. Choices use segments, chips or a menu and commit on selection. Colours use RGB fields or a swatch that opens an HSV plane, hue rail, RGB fields and hex entry. Curves show the module's sampled curve, editable points, channel selection and a numeric point list. Drag a point, double-click to add one, or use Delete to remove the selected point; the module's limits still apply. Changing channels changes only the view. A module's canvas picker button enters its picking mode and stays lit while selected; clicking it again returns to the pointer.

Slider, stepper, colour picker and curve gestures whose field is a whole request (a patch action's field or an action's only parameter) open one draft, preview each new value as soon as the previous one has answered, and commit once on release. Escape cancels the gesture. Fields commit on Enter; toggles, choices and action buttons commit once. Tab follows the generated control order. Right-click any generated control to inspect its action/parameter and copy the JSON request with the current revision.

The Controls proof in developer mode demonstrates this vocabulary without changing photo pixels. Its edits still create normal history entries; `edit.set-controls` changes one field and `edit.reset-controls` restores the defaults. Tone Curve, Detail and the colour mixer are not photo tools yet. Text and 2D pad controls are deferred.

### Developer components gallery

Debug builds show a **Developer** button in the title bar. For the optimized development build,
start with `cargo xtask develop --developer`. Click Developer to browse the ten pages of shared
UI components, including their disabled, editing and dragging states. Use the page chooser or
Previous/Next, then **Back to editor** or Escape to return. A photo is not required; an open photo
and its edits are preserved. Finish an active draft before opening the gallery.

These are reference previews. For live control interactions, expand the Controls proof in the
tools panel in developer mode. Gallery navigation is also exposed as
`workspace.set {"component_gallery": 0}` (pages 0–9); use `null` to return to the editor.

### Presets

Presets is the first section of the tools panel, collapsed until you open it. A preset is a named set of Basic, Presence, Colour mixer and Vignette settings. Click one to apply it: the settings it holds replace the photo's values for those settings, every setting it does not hold keeps its value, and the change is one history entry, "Preset: Soft film", which Undo reverses as a whole. Applying a preset that changes nothing adds no entry. Presets never hold RAW development, transforms or crop, which belong to one photograph.

Choose + to save the displayed entry's settings as a preset. Give it a name and a group (User presets unless you type another), and tick the groups of settings it should hold. Every group starts ticked except White balance, because white balance usually belongs to one photo; tick it to include it. A preset saves every value of the groups you tick, including those at their defaults, so applying it resets them. Names are unique within a group, ignoring case.

With a photo open, choose Import to add a preset file: a Lightroom Classic develop preset (`.xmp`), a legacy Lightroom template (`.lrtemplate`) or a Lightwell preset (`.lwpreset`), up to 1 MiB. The status bar reports how many of the file's settings were carried over and how many were not; Copy copies the message. Lightroom settings are carried over as values on the Lightwell controls with the same name, range and direction, such as Exposure, Contrast, Clarity, the colour mixer and the post-crop vignette. The numbers transfer, but Lightwell's processing is its own, so an imported preset looks similar, not identical. A preset whose settings could not all be carried over shows **Partial**; its tooltip gives the counts, and right-click › Copy import report gives every setting with the reason. Lightwell does not carry over:

- tools it does not have: tone curves, colour grading, sharpening, noise reduction, grain, lens corrections, profiles, masks and crop;
- Lightroom's RAW Kelvin and tint, whose scale is not Lightwell's;
- values outside Lightwell's ranges, which are never clamped;
- presets from Lightroom's 2003 and 2010 processes.

Settings at their neutral value, such as grain at 0, lose nothing and are listed separately. Lightroom profiles and files with nothing Lightwell can apply are refused. The imported file is kept in the catalog, so nothing in it is lost.

Right-click a preset to delete it or to export it as a `.lwpreset` file that another Lightwell catalog can import. Cmd+K lists "Apply preset: <name>" for every preset. Presets live in the catalog beside history, so a new catalog starts with none.

### Basic

Basic follows Presets in the tools panel, in three groups.

**White balance** is Temperature and Tint, each −100 to +100 in whole steps. Positive temperature warms the photograph and negative cools it; positive tint is magenta and negative is green. They correct the rendering the JPEG already has — they are not the camera's Kelvin value, which a JPEG does not carry and nothing here recovers. The group header reads **Original** while both are zero and **Custom** once either is not, and every group reads its own state the same way.

**Tone** is Exposure, Contrast, Highlights, Shadows, Whites and Blacks. Exposure runs from −5.00 to +5.00 EV in hundredths and multiplies the photograph's linear-light channels by `2^EV`. The other five run −100 to +100 in whole steps and compose one global luminance curve: Whites and Blacks move the endpoints, Highlights and Shadows reshape the bright and dark ranges, and Contrast separates the midtones about a fixed pivot. Being a global curve, lifting deep shadows loses some local contrast; an edge-aware stage is a later proposal, not a missing setting.

**Colour** is Vibrance and Saturation, each −100 to +100 in whole steps. Saturation scales colour intensity uniformly and −100 is exact neutral grayscale; Vibrance raises near-neutral colour more than colour that is already strong, with less gain in a skin-like hue band — a colour heuristic, not face or skin detection.

Whatever order you touch them in, they are evaluated in a fixed one: white balance, then exposure, then Tone, then Vibrance, then Saturation. All of it is one layer in the recipe, placed before the quarter-turns and the crop the first time a value leaves zero, and updated in place ever after.

Dragging Exposure previews live. The drag opens a draft, the canvas keeps up with it while you move, and the status bar says which control you are drafting; nothing is written until you let go. Release commits the whole gesture as one history entry, labelled with the value you landed on — "Exposure +1.00 EV" — which is the value the slider was showing, not a longer number behind it. A drag that ends where it started commits nothing at all. Press Escape to discard a drag and put the slider and the photograph back to where they were. Holding an arrow key over the slider's rail steps it and commits once when you let the key go; the arrow keys reach the slider only while the pointer is over that rail. Clicking the value and typing a number commits that one value when you press Enter, with no draft at all.

If somebody else — another client, or your own undo — commits while you are dragging, the Changed elsewhere card appears over the canvas and your drag is kept, not thrown away. Discard drops it; Reapply rebases it on the new state, keeping the value you had chosen and putting your preview back over it. Until you choose, the drag will not commit.

Each group header has its own reset, which returns that group's fields to zero as one entry — "Reset White balance", "Reset Tone", "Reset Colour" — and leaves the other groups exactly as they were. The section header's reset returns the whole module, "Reset Basic". Double-clicking one control's label, or the slider itself, resets that one field. All of them keep the Basic layer with its identity and a neutral payload, so a reset is an edit you can undo, not a deletion, and a reset of something already neutral does nothing at all. A Basic drag and a crop draft cannot be open at once: starting one while the other is open is refused, and the status bar says so.

#### The neutral picker

Click Neutral picker in the White balance group, or press `W`, and click something in the photograph that should be neutral grey. Lightwell reads a 5 × 5 patch of pixels around where you clicked, works out the Temperature and Tint that make that patch neutral, and applies them as one edit. It reads the patch as the image is *before* Basic's own white balance, so clicking the same spot twice answers the same way however strong a correction is already set, and the picker stays selected so you can try another spot; clicking Neutral picker again, `V`, Escape or another mode leaves it, and leaving commits nothing.

Some patches cannot answer, and the status bar says which:

| Reason | What it means |
| --- | --- |
| `clipped:` | A sampled pixel is at 0 or 255, so the patch has no usable colour left |
| `near-black:` | The patch is too dark for its colour to be trusted |
| `out-of-range:` | The correction this patch needs is outside what Temperature and Tint can express; it is reported rather than quietly clamped |
| `outside the stage:` | The point is not inside the image |

A refusal commits nothing and changes nothing on screen. The picker is refused, with the reason, while a slider drag or a crop draft is open: finish or discard that first.

Pixel proof lives in the Developer section in developer mode. It accepts integer x/y coordinates and RGB values from 0 to 255. Coordinates are content coordinates: the photograph after EXIF orientation, with a top-left origin, x right and y down, regardless of any crop or rotation applied afterwards. Clicking the photo at Fit or any zoom fills X and Y with the content pixel shown under the pointer without committing anything, so a click in the corner of a cropped view names the pixel of the photograph that is drawn there. Changing the crop later never moves an edit; it only changes what is visible. Apply pixel creates one layer and one attributed history action when the resulting pixel changes. Invalid, out-of-bounds and same-value requests add nothing.

Exact transforms are Rotate left, Rotate right, Mirror horizontal and Flip vertical, the four icon buttons in the Transforms section; hover one for its name. Quarter-turns swap dimensions. Repeated transforms fold into one orientation layer while it is the last layer of the stack, so four Rotate right actions leave one neutral layer and four history actions to undo through; a transform made after a crop starts a new orientation layer so the crop is carried with it. Pixel edits always sit before the transforms and crop, so they move with the image.

Fit shows the whole image, centred with a margin of canvas around it, rendered at the size your display can show so that dragging a slider redraws it immediately; the full-resolution render still runs behind it for the histogram and the clipping overlays. Type a percentage from 10 to 1600 in the field beside it and press Enter, or choose 100%; neither Fit nor 100% is selected while a typed percentage is in force. On a high-DPI display, 100% maps one source pixel to one physical framebuffer pixel of the exact full-resolution render. Both scroll axes pan content larger than the viewport.

Hold Compare in the title bar, or hold `\`, to see the Original entry for as long as the key or the button is down; releasing returns to whatever was selected before, without touching history or anything else in the session. Compare is refused while a crop draft or a slider drag is open, and the status bar says so.

A strip floats under the photograph with the pointer, one entry per registered module that declares a canvas mode, and the Thirds overlay. Thirds draws two guides each way over the fitted photograph; at a percentage zoom, and while a crop draft is open, the overlay is left to the crop rectangle's own guides.

Cards appear over the top of the canvas when something needs saying: a draft or a slider drag that was changed elsewhere, a preview that is stale because a stored layer's module is unavailable, an original that cannot be found, or a rendering limit. They name the cause and offer only the actions the core allows; none of them blocks the rest of the screen.

### Histogram and clipping

The histogram sits at the top of the tools panel. It describes the **rendered output** of the photograph you are looking at: the whole composition after the crop and every edit, in 8-bit sRGB, as the plot says when you hover it — "Output · sRGB · after crop". It is not a camera or RAW histogram, so an endpoint count here is a warning that the finished picture has flat black or blown white, not a statement about what the sensor recorded or about detail that could be recovered. Panning, zooming and your display's scale never change it; selecting a history entry or a different photograph does, and so does dragging a slider — while a gesture is open the plot, the counts and the clipping overlays describe the drafted picture on the canvas, not the last one you committed; the plot is dimmed until the full-resolution render of the value you paused on has finished, and the overlay follows the drag from the picture on screen until then.

The three channels are drawn as filled, overlapping shapes on one shared vertical scale, so red twice as tall as green really does mean twice as many pixels. A grey area is the three channels overlapping, not a separate brightness curve. The histogram is just the plot and the two triangles under it, always the same height, so nothing in the tools panel moves while you drag, move the pointer or wait for a render. While a new version of the picture is still rendering the previous plot stays on screen, dimmed. If the picture cannot be rendered at all, or nothing has been analysed yet, the plot says so in its middle — "No analysis yet", or "Unavailable:" with the reason — instead of showing an empty plot.

The two small triangles in the plot's bottom corners are the clipping overlays, shadows on the left and highlights on the right. A triangle takes its colour when that endpoint has any pixels; click it to draw that overlay on the photograph. Blue marks a pixel with any channel at 0, red a pixel with any channel at 255, and magenta a pixel that is both at once — the rule each triangle states when you hover it. Under the rule, the tooltip writes the counts out: the shadow triangle's says how many pixels have red, green or blue at code 0, how many have any channel there and how many have all three; the highlight triangle's says the same at code 255 and how many pixels are at both endpoints at once. Without an analysed picture the counts show a dash rather than zeros that would read as "nothing is clipped". `J`, and the Clipping button in the title bar, turn both overlays on and off together. The overlay is drawn over the photograph and changes nothing else: not the pixels, not the saved recipe, not the histogram's own counts, and not a future export. A single clipped pixel still lights its own mark when the photograph is shown fitted, so an isolated blown highlight is not lost in the reduction.

Move the pointer over the photograph and the three output codes under it appear in the status bar, with the pixel's coordinates: `R 128 · G 64 · B 255 · 120, 80`. It reads the same evaluated picture the plot describes, clears when the pointer leaves, and asks for one pixel at a time however fast you move. It has a fixed place in the status bar, so nothing else there moves as it appears and clears.

### Presence

Presence is the section under Basic, collapsed until you open it, with three sliders that each run −100 to +100 in whole steps. They act on a neighbourhood of every pixel rather than on the pixel alone, which is what separates them from Basic's Contrast.

**Texture** raises or lowers medium-scale detail: fabric weave, foliage, skin texture. Positive values strengthen it and negative values soften it while broad shading stays where it was. **Clarity** does the same for broad local contrast, the difference between a region and its surroundings, so a flat scene gains depth and a harsh one calms down; it works on brightness only, so colours keep their hue and saturation, and its gain is compressed near black and white so it cannot create a new clipped highlight or crushed shadow. **Dehaze** estimates the atmospheric veil in the photograph, the brightest low-contrast light the haze adds, and removes a fraction of it at positive values or adds a uniform veil at negative ones. It changes colour as well as brightness, because haze is a coloured veil, and it can only remove what the estimate finds: a veil that varies quickly across the frame is partly left in place.

Whatever order you touch them in, they run in a fixed one: Dehaze, then Texture, then Clarity, after Basic and the Colour mixer and before the quarter-turns and the crop, as one layer updated in place. Each slider drafts and previews live as Basic's do and commits once on release with a label such as "Clarity +40"; the section's reset returns the three to zero as one entry, "Reset Presence". A neighbourhood operation costs more than a pointwise one, and the three together cost more than their sum, because every tile of the photograph is computed with a wide margin around it; at Fit the drafted preview is rendered at the display's size, so it follows the drag, and it is an approximation of the exact result there: the neighbourhoods scale with the picture on screen, so fine texture in particular reads differently at Fit than at 100%, where the exact full-resolution render is shown. Every number (the histogram, the clipping overlays, the readout) still comes from the exact render, and a committed edit is always the exact one. Fine noise is texture as far as these controls can tell, so both amplify noise in shadows; inspect at 100%.

### Colour mixer

The Colour mixer is the section after Presence, collapsed until you open it. It adjusts eight colour ranges separately: red, orange, yellow, green, aqua, blue, purple and magenta, in that order around the wheel, each with a **Hue**, **Saturation** and **Luminance** slider from −100 to +100 in whole steps. The three properties are three tabs over the same eight ranges, Hue selected first; a dot on a tab marks a property you have changed, so edits on a hidden tab stay visible, and the reset at the right of the tabs resets the visible property. Choosing a tab only changes what you see: it adds nothing to history.

For Saturation and Luminance a pixel belongs to at most two neighbouring ranges, in proportion to where its hue sits between their centres, so a change to Red fades smoothly into Orange and Magenta and never draws a hard edge through a gradient. Hue turns a range's colours toward the next range (positive) or the previous one (negative): at ±100 the range's own colour travels 85% of the way to its neighbour's, so Blue −100 turns blues teal and Yellow +100 turns yellows green. The hues around it bend smoothly to make room, out to the ranges beyond its neighbours, and never cross; two neighbouring ranges pushed toward each other share one slider's travel. Greys and near-greys are never turned, lightened, darkened or coloured, which keeps shadow noise clean. Saturation runs from fully grey at −100 to twice the colour's chroma at +100, and with all eight Saturation sliders at −100 the whole photo is exactly black and white. Luminance darkens or lightens the range's colours without pushing them past black or white. The rail under each slider shows what it does: the hue rail runs from the previous range's colour through this one to the next, the saturation rail from grey to the colour, the luminance rail from its dark to its light version.

It is one layer in the recipe, always evaluated after Basic whichever you touched first, and each slider drafts, previews and commits as Basic's do, labelled with the range and property: "Red hue +20", "Reset Hue", "Reset Colour mixer". Lightroom's per-colour view, targeted adjustment drag and black-and-white mix are not built.

### RAW development

The NEF, RAF or DNG remains the original throughout editing. Exposure, white balance and composition are saved as recipe settings. History and versions retain those settings; reopening rebuilds the needed high-precision image from the original. Display previews are disposable.

The RAW panel appears before composition tools for a RAW asset. Dragging Exposure shows the change on the photograph as you drag. Dragging Custom temperature or Custom tint does too, approximately: an exact new white balance needs the sensor data redeveloped, so while you drag Lightwell previews it on the image it has already developed, and the status bar's render time says "approximate". The histogram keeps the last exact result, dimmed, while you drag. When you let go, the sensor data is redeveloped for the value you landed on — about half a second on the Z6, longer on larger files — and the exact photograph and its histogram replace the preview, which stays on screen until then. The preview is within a code or two of the exact result almost everywhere; fine high-contrast edges, specular highlights and strongly saturated colours can differ by more, most for a large change on the Air 2S. Each drag records one history entry when you let go. Exposure spans −5 to +5 EV. As shot uses the captured camera-channel gains. Custom temperature spans 2000–12000 K; Custom tint spans −100 to +100 Lightwell units, with positive values making the result more magenta. Their initial 6504 K/0 values are a new custom target, not a measurement of As shot. Changing either control stores resolved sensor gains. These units do not promise numeric equivalence with another editor. Double-clicking a slider's label or rail resets that one field, as Basic's do: Exposure to 0 EV, Custom temperature to 6504 K and Custom tint to 0, which is a custom white balance rather than As shot. A double-click on the rail first moves the value to where you pressed, which is an entry of its own, and the reset follows once that entry is saved; for temperature and tint that takes as long as the redevelopment.

Click Neutral WB in the RAW section, under Custom tint, or press N, then click a neutral surface. It is the RAW picker, distinct from the Basic panel's own Neutral picker on `W`, which corrects the rendered image rather than the sensor. The picker maps the point back to the original sensor and averages a bounded patch before WB/exposure. A dark, clipped or unusable patch reports an error and commits nothing. As shot restores captured WB while keeping exposure; Reset RAW restores both source adjustments. Every successful change remains undoable. A WB change redevelops the retained mosaic; exposure and geometry reuse the prepared float image. The previous rendition stays visible during preparation.

The neutral rendition uses camera calibration without film simulations, Picture Controls or automatic brightness. Air 2S applies its required embedded optical corrections and uses fixed daylight calibration for custom Temperature/Tint; additional lens profiles and dual-illuminant color-profile interpolation are not implemented. Sensor headroom remains available to exposure changes even when the current display is clipped. Camera crop metadata and EXIF orientation determine the initial frame.

### Keyboard

Letters act only when no text field has focus. `F` fits, `1` is 100%, `O` toggles the thirds overlay, `J` toggles both clipping overlays, `V` returns to the pointer, and each module's declared letter (`R` for crop and straighten, `W` for the Basic neutral picker, `N` for the RAW sensor picker) enters its canvas mode, exactly as the mode strip and the pickers in the tools panel do. Escape leaves a canvas mode that has no draft of its own. `\` holds Compare. Cmd+Option+[ and Cmd+Option+] show and hide the two side panels. Cmd+O / Ctrl+O opens a file, Cmd+Z / Ctrl+Z and Shift+Cmd+Z / Shift+Ctrl+Z undo and redo, and Tab and Shift+Tab move between fields.

### Crop and straighten

The crop editor appears in the tool panel as soon as a registered module declares a crop frame; the ratios, the angle range and the action it commits all come from that module's descriptor.

Choose Crop to open a draft. The surface then shows the crop layer's own input stage, which is the stack rendered up to but not including that layer, so layers after an existing crop are not drawn while you adjust it. Everything outside the crop rectangle is dimmed, and the rectangle carries a thirds overlay, a border and eight handles.

- Drag a corner handle to move two edges, or a side handle to move one with the opposite edge fixed. Drag inside the rectangle to move the composition; it slides along a boundary instead of stopping. The rectangle never leaves the photograph, so a crop never has an empty corner, and it never shrinks below one pixel on either axis.
- Hold Option (Alt on Windows and Linux) while dragging any handle to apply one scale factor about the fixed centre, keeping the current width to height even with no ratio locked. The centre does not move.
- Choose a ratio preset to keep the centre and fit the largest rectangle of that ratio inside the current one. Custom takes the two extents typed beside it. Lock ratio pins whatever the rectangle currently is, Unlock ratio releases it, and Swap inverts a locked ratio the same way.
- Drag the angle's rail between the −0.5° and +0.5° buttons, which moves in 0.05° steps, or use the buttons, or click the angle's box, type an angle from −45 to 45 degrees and press Enter. Every angle is measured against the last handle, move or ratio change, so sweeping the angle away and back returns exactly the rectangle you had. Larger rotations are the quarter-turn transforms.
- Turn Straighten guide on and drag a line along something that should be level; releasing rotates by the angle that makes that line horizontal or vertical, whichever is nearer.
- Hold Space and drag to pan at a percentage zoom. Fit, 100% and a typed percentage all work while drafting, and 100% still shows one input pixel per physical pixel.

The panel prints the input stage, the rectangle in whole box pixels, the resulting output size and the exact values the draft would commit.

Apply, or press Enter, commits one action and one new history entry, adjusting the existing crop layer in place and keeping its identity or appending one when there is none. Cancel, or press Escape, discards the draft and changes nothing. Enter and Escape act only when no field has just consumed the key. Reset crop is the module's own control: it commits the neutral crop through history and ends the draft. No pointer movement commits anything, and the rotation shown while drafting is a display filter — the committed render is what counts.

Selecting a historical state pauses the draft rather than discarding it: the historical preview is shown and Return to current resumes drafting. If anything else changes the photograph while a draft is open — another client, or your own Undo, Redo or Restore — the draft is kept and marked "Changed elsewhere". Apply is refused until you choose Discard, which drops the draft, or Reapply, which re-reads the current stack, rebases the draft onto it keeping the angle and the composition as far as it fits, and lets you apply normally.

### Vignette

Vignette is the last section of the tools panel, collapsed until you open it, and it is applied after the crop: its centre is the centre of the cropped picture, and a later crop moves it, exactly as a post-crop vignette should. **Amount** runs −100 to +100; negative darkens toward black at the edges, and −100 reaches black at the corners, while positive lightens toward white without ever exceeding it. **Midpoint** (0 to 100) sets how far from the centre the falloff begins, **Feather** (0 to 100) how wide the transition is, from a hard edge at 0 to a gradient that spans the whole picture at 100, and **Roundness** (−100 to +100) the shape, from a rounded rectangle through the ellipse that matches the picture's proportions to a circle. The corners are always the far edge of the mask, whatever the shape.

Amount 0 is the identity whatever the other three hold, so a layer with the amount at zero changes nothing. Each slider drafts and commits as Basic's do, labelled "Vignette amount −35" or "Vignette feather 80"; the reset returns all four to their defaults as one entry, "Reset Vignette". Only this one style is built: the highlight-priority, colour-priority and paint-overlay variants are not.

## History

History lists Original and every committed action newest first, with its sequence, a label and the actor. The label comes from the action's declared summary ("Crop 16:9", "Rotate right") or its title, and is stored with the entry. The filled accent marker is current and an accent outline is the previewed entry; selecting the current row keeps you on the live state, and selecting another row previews that immutable snapshot without changing current state or revision, the status bar says which entry is shown, and Return to current and Restore appear under the list. Restore appends a new action containing the selected recipe. Editing is disabled during a historical preview.

The recipe block lists the displayed entry's layers in processing order with the module title and the module's own summary of each layer, from `recipe.describe`; a layer whose module is unavailable shows the reason instead. Each row also carries the parameter `values` that layer represents, when its module reports them, so a client can show the settings of the entry on screen.

Versions are chips naming saved states. Choose + to reveal the name field and Save; each chip carries its entry number, selecting one previews it, and Restore brings it back as a new action. Right-click a chip to delete the name; the entry stays in history. History rows marked branch were undone and replaced by later edits; they remain available for preview, restore and versions.

Undo and Redo navigate saved states without appending rows. Cmd+Z / Ctrl+Z and Shift+Cmd+Z / Shift+Ctrl+Z invoke the same service as the buttons. A new edit clears shortcut redo while every older entry remains available for preview or Restore. Layers, history, navigation state and stable IDs survive reopening the catalog.

Missing or changed sources keep their catalog data and report why rendering is unavailable. Only the current catalog format (5, which stores the preset library as well as typed source interpretation and entry labels) and operation formats are supported during pre-release development. Unsupported formats fail explicitly; Lightwell never silently resets or drops them. If a catalog format is rejected, start with a new path using `--catalog /path/to/new-catalog.sqlite` and import the originals again.

## JSON automation

The headless owner reads one JSON request per line and writes one response per line. Diagnostics stay off stdout:

```sh
target/release/lightwell-json --catalog /path/to/catalog.sqlite
```

Keep the process and its input open while requests are in progress. For example, send:

```json
{"id":"schema","method":"schema.list","params":{}}
{"id":"import","method":"catalog.import","params":{"path":"/path/to/photo.jpg"}}
```

Import returns a job acknowledgment. Poll `job.status` with the returned `job_id` until it is `ready` or `failed`; a ready result includes the asset state. `job.adopt` adopts the client's latest ready import into its session. `job.cancel` removes that client's interest in a job; another client's use of the same source continues. Ending the connection cancels its pending work.

A source-dependent request after reopen can return `preparation-required` with `error.job_id`. Wait for that job and retry against the current asset revision. `source.prepare` also allows explicit preparation. Loading, hashing, decoding and RAW development run on a bounded worker so other catalog requests can continue.

A request has `id`, `method` and `params`. A success carries the matching `id`, an event `sequence` and `result`; a failure carries a structured `error`. Start with `schema.list` for the authoritative method list and `catalog.list` for the referenced assets. Edit actions are generated from the registered tool modules: `module.list` returns every module with its effects, actions, parameter descriptors (kind, range, unit, default), semantic controls, hint, reset action, canvas title and shortcut, summary templates and developer flag, and each action `<id>` is callable as `edit.<id>` with its parameters as top-level fields beside `asset_id` and `mutation`. `recipe.describe` lists an entry's layers with each module's summary; `workspace.set` and `session.state` carry the per-client panels, canvas mode, thirds overlay and clipping overlays beside the view; every history entry carries its rendered `label`. Today that is `edit.set-pixel` (`x`, `y`, `rgb`), `edit.set-basic` (`temperature`, `tint`, `exposure`, `contrast`, `highlights`, `shadows`, `whites`, `blacks`, `vibrance`, `saturation`), `edit.reset-basic`, `edit.transform` (`transform`), the crop module's three actions and the RAW module's development actions. Pass `asset_id` to `module.list` to request only applicable modules; `source.inspect` reports source interpretation and readiness. A module may also declare read-only **queries**, which are generated the same way as `query.<id>`; they take `asset_id`, an optional `entry_id` defaulting to the session's selection, and their own parameters, and they write nothing. The controls proof adds `query.sample-controls-curve` with the active channel's points when `--developer` is enabled; its curve interpolation belongs to that module. The Basic neutral picker uses `query.neutral-sample` (`x`, `y`). Parameters are checked against the descriptors before the module sees them, so every client gets the same structured validation error. A number parameter may declare a `step` and a display `precision` for the control that drives it; they are hints, and a request is never rounded to them. An action listed with `patch: true` takes whichever of its fields you name: the fields you send are validated, nothing declared is filled in, the module merges them over what it already stores, the entry records the fields as sent, and a patch that changes nothing writes no entry. `version.create`, `version.list`, `version.delete` and `history.lineage` cover named states and the undo-parent chain. The `preset.*` methods cover the preset library, and `edit.apply-preset` applies one (below). `render.sample` reads one rendered pixel and `render.locate` maps a rendered pixel back to the content pixel it shows. Mutations require `asset_id` and a `mutation` object:

```json
{"id":"rotate","method":"edit.transform","params":{"asset_id":"asset-…","mutation":{"expected_revision":0,"request_id":"rotate-1","actor":"my-client"},"transform":"rotate-right"}}
```

RAW actions use the same mutation envelope:

| Method | Parameters |
| --- | --- |
| `edit.set-raw-exposure` | `ev` |
| `edit.set-raw-temperature` | `kelvin` |
| `edit.set-raw-tint` | `tint` |
| `edit.pick-raw-neutral` | `x`, `y` in upright original-content coordinates |
| `edit.use-as-shot-wb` | none |
| `edit.reset-raw` | none |

`render.locate` maps an edited-image point to the content coordinates needed by the picker. Explicit `edit.set-raw-red-gain` and `edit.set-raw-blue-gain` actions remain available to programs; each preserves the other effective camera gain and accepts 0.01–32×. RAW actions reject JPEG assets.

`edit.crop-fit` (`aspect`, optional `aspect-width`/`aspect-height` with `aspect: "custom"`, `angle`, optional `center-x`/`center-y`) fits the largest rectangle of a ratio about a center without computing the box geometry by hand:

```json
{"id":"crop-169","method":"edit.crop-fit","params":{"asset_id":"asset-…","mutation":{"expected_revision":3,"request_id":"crop-1","actor":"my-client"},"aspect":"16:9"}}
```

`edit.crop` (`angle`, `x`, `y`, `width`, `height`) sets the straightening angle and rectangle exactly as persisted, normalized to the rotated box:

```json
{"id":"crop-exact","method":"edit.crop","params":{"asset_id":"asset-…","mutation":{"expected_revision":4,"request_id":"crop-2","actor":"my-client"},"angle":0,"x":0.1,"y":0.1,"width":0.8,"height":0.6}}
```

Both actions reject a rectangle that would need an empty corner with a structured `validation` error naming the offending corner, its mapped input coordinates and how far outside the input stage it lands. Every crop request updates the stack's one crop layer in place, keeping its layer ID, or appends one when the stack has none; a request equal to the saved payload is a no-op. `edit.crop-reset` takes no parameters, returns an existing crop layer to the neutral payload (`angle 0, x 0, y 0, width 1, height 1`) and is a no-op without one.

`edit.set-basic` is a field patch over the Basic adjustments. `temperature` and `tint` are −100 to +100 in steps of 1, shown as whole numbers: positive temperature warms the image and negative cools it, positive tint is magenta and negative is green. They are a relative correction of the image's existing rendering — not the camera's Kelvin value, which is not recoverable from a JPEG. `exposure` is −5.00 to +5.00 EV in steps of 0.01, shown to two decimals, multiplying the image's linear-light channels by `2^EV`; it corrects a rendered sRGB JPEG, not scene-linear RAW data, so it cannot recover detail a clipped plateau no longer holds. The five Tone fields `contrast`, `highlights`, `shadows`, `whites` and `blacks` (each −100 to +100, step 1, no decimals) compose one frozen global luminance curve — Whites/Blacks, then Highlights/Shadows, then Contrast — documented in [Basic Tone](design/basic-tone.md). `vibrance` and `saturation` are each −100 to +100 in steps of 1, shown to zero decimals, scaling Oklab chroma about the achromatic axis: `saturation` uniformly, and `−100` is exact neutral grayscale, not merely a strong desaturation; `vibrance` with more gain for near-neutral colour than for colour already close to the sRGB gamut edge, and reduced gain in a skin-like hue band as a colour heuristic, not detection. The internal order is fixed regardless of edit order: white balance, then exposure, then Tone, then vibrance, then saturation.

```json
{"id":"expose","method":"edit.set-basic","params":{"asset_id":"asset-…","mutation":{"expected_revision":5,"request_id":"expose-1","actor":"my-client"},"exposure":0.5}}
{"id":"warm","method":"edit.set-basic","params":{"asset_id":"asset-…","mutation":{"expected_revision":6,"request_id":"warm-1","actor":"my-client"},"temperature":20,"tint":-5}}
{"id":"tone","method":"edit.set-basic","params":{"asset_id":"asset-…","mutation":{"expected_revision":7,"request_id":"tone-1","actor":"my-client"},"contrast":20,"shadows":15}}
```

The first non-neutral value adds the stack's one Basic layer before the quarter-turns, reflections and crop, and every later value updates that same layer in place; a value equal to the stored one is a no-op. A field you do not name keeps its stored value. `edit.reset-basic` takes no parameters, returns that layer to neutral and keeps its layer ID, and is a no-op without one or when it is already neutral. `recipe.describe` reports the layer's `values`, so a client can seed its controls from the entry it displays.

### Presets

`preset.list` returns the library, sorted by group, then name, each record with its `id`, `name`, `group`, `settings`, `origin`, the counts of its import `report` (`null` for a preset made in Lightwell) and any `unavailable` actions. `preset.read {preset_id}` adds the full report and the imported file's text. A settings set maps field-patch actions to the fields each one sets, for example `{"set-basic": {"exposure": 0.35, "contrast": 12}, "set-vignette": {"amount": -18}}`. Apply one with `edit.apply-preset`, which takes the set, a `name` for the history label and optionally the library `preset-id` as provenance:

```json
{"id":"preset","method":"edit.apply-preset","params":{"asset_id":"asset-…","mutation":{"expected_revision":8,"request_id":"preset-1","actor":"my-client"},"name":"Soft film","preset-id":"preset-…","settings":{"set-basic":{"exposure":0.35,"contrast":12},"set-vignette":{"amount":-18}}}}
```

The host runs each named action exactly as that action would run alone, in alphabetical order of the action names, and commits the result once as one entry, "Preset: Soft film". An unknown, non-patch or unavailable action, or a field its action refuses, refuses the whole preset and writes nothing.

| Method | Parameters | Does |
| --- | --- | --- |
| `preset.capture` | `asset_id`, `fields`, optional `entry_id` | Reads a settings set from an entry's stack (default: the displayed one). `fields` maps each action to an array of parameter names or `true` for all of them. Without a layer, a field takes its default |
| `preset.create` | `name`, `settings`, `actor`, optional `group` | Saves a preset; the group defaults to `User presets` |
| `preset.update` | `preset_id`, `actor`, optional `name`, `group`, `settings` | Renames, regroups or replaces the settings |
| `preset.delete` | `preset_id` | Removes it; a no-op when absent |
| `preset.export` | `preset_id` | Returns `file_name` and `content`, a `.lwpreset` document |
| `preset.inspect` | `content`, optional `file_name` | Returns the preset and report an import would produce, storing nothing |
| `preset.import` | `content`, `actor`, optional `file_name`, `name`, `group` | Imports a Lightroom `.xmp` or `.lrtemplate` preset or a `.lwpreset` document from its text; `name` and `group` override the file's |

A (group, name) pair is unique ignoring case, and a duplicate is a `conflict`. The library holds at most 1,000 presets. Create, update, delete and import emit events like every mutation, so another client's desktop refreshes its Presets section.

### The neutral picker

`query.neutral-sample {asset_id, entry_id?, x, y}` is the neutral picker, and it is a **query**: a read-only question about a stored stack that writes no history entry, emits no event and changes nothing. `x` and `y` are pixels of the content stage — the image after EXIF orientation, the stage the Basic layer receives — so map a pixel of the rendered image to them with `render.locate` first, exactly as the canvas mode does. `entry_id` defaults to the session's selection, so a client previewing a historical entry may pick in it.

It averages up to a 5 × 5 patch centred on that pixel in linear light, clipped at the stage's edges rather than clamped, evaluated **before** the Basic layer, and answers the temperature and tint that make the average neutral:

```json
{"id":"pick","method":"render.locate","params":{"asset_id":"asset-…","x":812,"y":544}}
{"id":"neutral","method":"query.neutral-sample","params":{"asset_id":"asset-…","x":1024,"y":768}}
```

```json
{"id":"neutral","sequence":6,"result":{"temperature":20,"tint":-5,"patch":{"x":1022,"y":766,"width":5,"height":5,"pixels":[[164,151,140]," … "],"mean_linear":[0.3706,0.3128,0.2664]}}}
```

Apply the answer with an ordinary `edit.set-basic` carrying its `temperature` and `tint`; that is the one entry the pick costs, and cancelling before you send it commits nothing. Because the patch is read before the Basic layer, picking the same spot twice answers the same way however strong a correction is already set.

The picker never guesses. A patch with a pixel at code 0 or 255, a patch too dark to carry reliable colour, a non-finite value, a correction the −100..100 axes cannot represent and a point outside the stage are each refused with a `validation` error whose message begins with its reason — `clipped:`, `near-black:`, `non-finite:`, `out-of-range:` or `outside the stage:` — and nothing is committed or clamped. In the workspace the picker is a canvas mode named Neutral picker, entered from the Neutral picker button in the White balance group, with `W` or from the command palette — the button is a declared `picker` control of the Basic module, and pressing it is one `workspace.set` — and it runs this same query with the same coordinates and the same solver.

A gesture that changes a setting over and over — a slider drag, a held arrow key — is a **draft**, so it costs one history entry instead of one per step. `draft.begin {asset_id, action}` opens this client's one draft and answers `{draft_id, action, asset_id, base_revision, draft_revision, fields, conflicted}`; `draft.set {draft_id, fields}` validates and merges the named fields against the action's parameter descriptors and counts up `draft_revision`; `draft.read {draft_id}` reads it back; `draft.cancel {draft_id}` ends it and commits nothing; `draft.commit {draft_id, mutation}` runs the action with the accumulated fields and ends the draft, returning the ordinary mutation result, so a gesture that returned to its start is a `no-op` with no entry. Only the commit changes the catalog; everything else is session state, reported by `session.state` and gone when the client disconnects. A draft is refused while this client already holds one, while a historical entry is previewed, and for an unknown action. Nothing is committed until the commit, and `render.sample {asset_id, x, y, draft_id}` evaluates the draft's settings so a readout during a gesture matches what committing would produce.

```json
{"id":"begin","method":"draft.begin","params":{"asset_id":"asset-…","action":"crop"}}
{"id":"drag","method":"draft.set","params":{"draft_id":"draft-…","fields":{"angle":2.5}}}
{"id":"release","method":"draft.commit","params":{"draft_id":"draft-…","mutation":{"expected_revision":5,"request_id":"straighten-1","actor":"my-client"}}}
```

`mutation.expected_revision` must be the draft's `base_revision`. If any client commits while the draft is open, the draft is kept and marked `conflicted: true`; committing it is then refused with a `conflict`. `draft.cancel` discards the gesture, or `draft.reapply {draft_id}` rebases it on the current revision and keeps only the fields this client set, so an unrelated field another client changed is retained by the commit that follows.

Read-only **analysis** answers "what does this image actually contain": an exact RGB histogram and the output clipping counters of one evaluated stack, with no GUI and without changing what the editor is showing. `analysis.request {asset_id, target}` returns `{job_id, status, identity}` right away, where `target` is `{"kind":"current"}`, `{"kind":"entry","entry_id":"entry-…"}` for a frozen historical entry, or `{"kind":"draft","draft_id":"draft-…"}` for this client's own open draft at the revision it holds now. `analysis.read {job_id}` returns `{status, identity, report?, error?}` and `analysis.cancel {job_id}` returns `{"cancelled":true}`; a job this client did not request is a `validation` error. None of the three changes anything or emits an event.

```json
{"id":"hist","method":"analysis.request","params":{"asset_id":"asset-…","target":{"kind":"current"}}}
{"id":"read","method":"analysis.read","params":{"job_id":"job-…"}}
```

`status` is `pending`, `ready`, `failed`, `superseded` or `cancelled`, and only `ready` carries `report`, so no state can be mistaken for a valid but empty histogram. A `report` holds three 256-entry arrays `r`, `g`, `b` of raw counts — each summing to the output pixel count — plus the endpoint counters `r0 g0 b0 r255 g255 b255 any_shadow any_highlight all_shadow all_highlight both`, the output `width`/`height` and the `domain`. These describe the **rendered sRGB output** after crop and edits; endpoint counts are output clipping warnings, not evidence about the original capture. The `identity` beside them names the asset, source fingerprint, entry, snapshot, the SHA-256 `recipe_hash` of the effective recipe, the `{draft_id, draft_revision}` when a draft was analysed, the output dimensions and the domain, so a result is never attached to the wrong image. One worker runs one job at a time with one replaceable pending job: a third request supersedes the pending one, which reads `superseded` and may simply be requested again. Identical requests from different clients share one job, and a cancel or a disconnect releases only that client's interest. `workspace.set` also accepts `clip_shadows` and `clip_highlights` booleans, reported by `session.state`; they are per-client view state and change no pixels, no saved recipe and no histogram, and they are the same two flags the histogram's triangles, the title bar's Clipping button and `J` set. Asking for the stack the desktop is displaying costs no render at all: the desktop's own preview worker reduces each frame it draws and hands the report to the owner, so a request for that identity is answered from it. The pointer readout uses `render.sample`, which evaluates one pixel of the compiled recipe and rasterizes nothing, so any client can read the same codes at any coordinate.

While the desktop owns a catalog it creates `CATALOG.live-session.json` beside it, recording a `127.0.0.1` address and a token. That session accepts the same newline-delimited requests with the token as the request's top-level `token`. The file is owner-readable on Unix and removed on orderly shutdown. Up to eight clients are accepted; requests are limited to 1 MiB and retained events to 256. Use `events.since`, and refresh with `asset.state` if it reports a gap.

A catalog has one owner. Starting the headless command against a catalog open in the GUI fails instead of creating competing state. Request IDs deduplicate retries; reusing one with different input fails.
