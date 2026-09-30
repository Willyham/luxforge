# Select, then develop: the catalog

Status: implementation in progress, on the recorded defaults of the proposals. This design covers how photographs get from a camera card or a folder into development, and how the photographs you develop are kept and found. Each choice the owner has not made is a numbered proposal with a recommended default under [proposals](#proposals). The implementation follows those defaults, but none is a decision until the owner accepts it and it is recorded in [product decisions](../decisions.md). The implementation plan is [tasks/catalog.json](../../tasks/catalog.json).

The contracts have landed:

- catalog format 12 ([storage](versions-and-lineage.md#storage-catalog-format-12));
- the index database;
- the shared types and every method's declared shape ([API](#api));
- generated data (`cargo xtask generate-catalog`);
- module skeletons.

Registered so far: browse views, facets, the selection and the event list (`browse.*`, `event.list`; [Views on the owner](#architecture)), which the desktop's Select workspace shell browses ([Workspaces](#workspaces)); picks and the journal of library changes (`pick.set`, `pick.list`, `library.journal`, `library.inspect`, `library.undo`, `library.redo`), `catalog.info`, and, over the built preview lane and cache ([the index and previews cache](#the-index-and-previews-cache)), `preview.read`, which nothing in the desktop uses yet. What the catalog does today is in [feature status](../features.md).

## Product

### What it is

Luxforge works in two steps.

1. **Select.** Browse the photographs on a camera card or anywhere on disk, very fast. Luxforge organizes them for you, without changing or copying anything: into **events** by date and place, then by day, camera and **moment**, where the frames of a burst or an exposure bracket sit together. Look at any frame full screen and at 100% to check focus, and **pick** the few worth developing.
2. **Develop.** "Develop 18" brings the picked photographs into the **catalog** and opens them in Develop, where the filmstrip holds that development set and nothing else.

The catalog is therefore the photographs you develop, not everything you shot. A thousand-frame trip from which you picked twenty adds twenty photographs to the catalog. The other 980 stay where they are on disk and are not Luxforge's to manage: the next time you browse that card or folder, Luxforge organizes it again.

This is deliberately unlike importing everything and culling inside the catalog, as Lightroom does. There, the library fills with every frame ever shot, flags and stars are the only way to find the few that matter, and the catalog, its previews and its backups grow with the shutter count rather than with the work. Here selecting is its own fast step, shaped around the decision that actually matters — which frame of this moment is worth developing — and the catalog stays the size of what you care about.

### The words

| Word | Means | Luxforge's to keep? |
| --- | --- | --- |
| **Filesystem** | Where your files are: the Mac's disk, external drives, camera cards | Never. Luxforge reads it and never writes to it |
| **Index** | What Luxforge has read from the files you browse — headers and embedded previews — so browsing is instant the second time | A cache beside the catalog. Deleting it loses nothing but time |
| **Event** | An automatic bucket of photographs taken close together in time and place ("Konstanz · 12–13 Sep"), computed from the index | Nothing to keep: computed, not created, named or deleted by hand |
| **Moment** | A burst or a bracket inside an event, computed the same way | Nothing to keep |
| **Pick** | A file you have marked to develop | Yes, until you develop it or clear it, so a long culling session survives a crash or a restart |
| **Catalog** | The photographs you developed: identity, fingerprint, recipe, history | Yes. This is what Luxforge is for |
| **Catalog folder** | Where a developed photograph lives in the catalog, one folder each: made from its event when you develop ("Konstanz · Sep 2026"), named then, and yours to rename, merge, nest and reorganize. Not a folder on disk | Yes |
| **Collection** | A set of developed photographs across folders, such as a portfolio | Yes |

There is no import and no "shoot" to create or manage. The only thing Luxforge keeps before you develop is your picks. Organization becomes yours at the moment you develop: an event, computed and disposable, becomes a catalog folder, which is kept.

### What selecting is for

- **Speed.** Hundreds of frames a minute. Frames appear as soon as their headers are read; stepping to the next frame never waits on a decode that could have been done ahead; nothing is developed to be looked at.
- **Sensible grouping without effort.** A camera card's `DCIM/100NZ8_1` tells you nothing. Events say where and when; days and cameras split them; moments gather what belongs together.
- **Moments, not a flat list.** A burst of six frames in 1.4 seconds is one decision, "which of these". A bracket of three exposures is a different decision: usually one photograph to merge, not three to choose between.
- **Detail on demand.** Fit to screen and a 100% focus check under the pointer, on any frame, immediately.

### Who it is for

- **The owner.** Copy the card to the internal SSD, or browse it in place; pick and develop the few; later move the folders to an external drive. Developed photographs keep their edits when their files move, and a whole folder is relinked in one verified step ([owner priorities](../decisions.md#owner-workflow-priorities): "full catalog with lazy shoot subsets", "bracket and panorama identification").
- **Photographers at large.** Mixed cameras on one trip, bursts from sport and wildlife, brackets for landscapes and interiors, drones and phones that record location. The design scale is 10,000 files in view browsing as fast as 100, and a catalog of 100,000 developed photographs, with 1,000,000 as the structural ceiling no data structure may make impractical.
- **Agents.** "Pick the sharpest frame of each burst in this event", "pick the middle exposure of every bracket", "add what I developed today to Portfolio": the same commands the desktop sends, attributed and undoable.

### Principles

- **References, never copies.** Luxforge never writes to, moves, renames, copies or deletes a file it browses or develops. "Develop" brings a photograph into the catalog; the file stays where it is.
- **Developing is the only way into the catalog.** A browsed file is not an asset. It becomes one, with a fingerprint, a recipe and a history, only when its pick is developed.
- **Selecting reads no pixels it does not need.** Browsing uses the files' headers and the camera's embedded previews, labelled as such. A frame is developed by Luxforge only when you develop it, or when a 100% check needs more detail than its camera preview has.
- **Organization is computed.** Events and moments come from capture time, place, camera body and exposure, with their rules recorded and adjustable; there is nothing to maintain.
- **The catalog is authoritative; the index and previews are a cache.**
- **Organizing is not editing.** Picks and collection changes are **library changes**: attributed, numbered, journaled and undoable, beside each photograph's history, never inside it.
- **Honest and programmable.** A preview says what it is; an offline file says so; every gesture is a command in the registry, and an agent's changes appear live.

### A day with it

1. The owner puts the Nikon's card in. Luxforge offers it ("NIKON Z 8 card connected · 612 photographs, organized into 3 events"); the owner has also copied the Leica's card to `~/Pictures/Card dumps/2026-09-12`, which is in an indexed folder. Under **Events** "Konstanz · 12–13 Sep" appears with 1,042 photographs from both cameras; within a second the first screen of frames is showing, grouped by day, camera and moment.
2. Space on a burst of six frames of a ferry fills the screen with the first frame. `→` steps through the six; `Z` shows the pointer's neighbourhood at 100%. Frame 3 is sharp: `P` picks it, and Luxforge moves on to the next moment.
3. A bracket of three frames is labelled a bracket: the exposure steps −2, 0, +2 EV in the metadata. The owner picks all three, to merge later.
4. After forty minutes, 18 of 1,042 frames are picked. "Develop 18" asks where they go — "Into the catalog folder: Konstanz · Sep 2026 (new)" — and notes that 12 of them are still on the card but already copied, so it will use the copies. Return accepts. The eighteen open in Develop; `→` moves through them, each appearing at once from its preview while its original prepares.
5. Next week an agent adds the eighteen to "Portfolio › Landscapes"; the change is in the journal with its actor.
6. In the spring the folder is on "Photos SSD", unplugged. The eighteen developed photographs are still browsable; the event shows its cached previews and says its files are offline. After the drive is reorganized, Missing originals shows the eighteen under the folder they came from; Find in a folder… on the drive's new layout finds and verifies each one, and Relink 18 points them there.

## How it works

### Workspaces

A segmented control at the title bar's leading edge switches between **Select** (`G`) and **Develop** (`D`). Select browses files and the catalog; Develop edits the development set. Switching never commits, discards or pauses anything, except that a Develop draft must be applied or cancelled first, under the [one start refusal](develop-workspace.md#interaction-rules).

Select's layout ([event board](catalog/event.png)) uses the Develop workspace's tokens and density:

| Region | Size | Contents |
| --- | --- | --- |
| Title bar | 44 pt | Workspace switch; the view's name and a summary (dates, count, cameras, where the files are); Add a folder…; Undo and Redo of library changes; **Develop N**, the one primary action; panel toggles |
| Sources panel (left) | 240 pt | A search field (places, dates, cameras). **Cards**: camera cards currently mounted. **Events**: by month, each with its picks and total, a hollow dot when its files are offline. **On disk**: the filesystem by volume, for browsing any folder directly. **Catalog**: All photographs, Recently developed, catalog folders by year, collections, Missing originals, Removed. Performance pinned at the bottom, as in Develop |
| Centre | remainder | Over files: All, Picked, Moments without a pick, with Camera, Kind and Group chips. Over the catalog: search, filter chips and the metadata browser. Then the grid or the loupe; a floating strip with Grid, Loupe, sort and size |
| Info panel (right) | 300 pt | For a file: preview, **Pick**, **Moment** (kind, why it was grouped, its frames, Compare frames), **Metadata** (including place). For a developed photograph: **Organize** (its catalog folder, its collections), **Metadata**, **Develop**. For several: the batch form |
| Status bar | 26 pt | What last happened, with Undo when it was a library change; "Camera previews · auto-organized" over files; counts in view, picked and selected |

**Built so far** (TASK-019, first part). The switch is in both title bars; `G` in Develop shows Select, and `D` in Select waits for picks. Which workspace is shown is the desktop's own view state, like the developer gallery page: `session.state` does not carry it, and evidence records it with every captured frame. Switching to Select answers the one start refusal's one-draft half ("Apply or Cancel the crop draft before switching to Select"); nothing else holds it back, and each workspace keeps its state. Select draws its title bar (the view's name and summary; Add a folder…, Undo and Redo shown disabled with a tooltip until their lanes land; Develop N from the summary's picks, inert until developing picks lands; the two panel toggles), the sources panel (the search as `event.list`'s `query`; Events by month with dates, picks over total and the offline dot; On disk's Browse a folder…, the native folder dialog viewing that folder with its subfolders until the volumes land; Catalog's All photographs, Recently developed, Missing originals and Removed with blank counts until `catalog.info`; Performance pinned under it), the filter bar (the pick segments with Picked's count from the Pick facet, Camera and Kind chips whose menus come from `browse.facets`, the Group chip), the grid over the view with the floating strip (Grid; Loupe disabled; the sort; the size slider), the Info panel (the active item's placeholder, Moment and Metadata bands, or the count selected) and the status line. Over the catalog the view is sorted newest first and ungrouped in the catalog's larger cells, with no pick segments or Group chip. Not yet: previews (cells are the placeholder at the photograph's shape), picks and the moment headers' pick counts and Pick all, the loupe, Develop N and its confirmation, Add a folder… and cards, library undo, the catalog's counts, folders by year, collections and its search and metadata browser, and the Missing originals row's red count.

### Browsing the filesystem

- **Cards.** A mounted volume with a `DCIM` folder is offered under Cards and with a notice. Browsing it reads it in place.
- **On disk.** The filesystem, by volume: open any folder to browse its photographs, with the same grouping. It is named so that it is never confused with catalog folders.
- **Indexed folders.** **Add a folder…** (`Cmd+O`, the palette, or a drop on the window) adds a folder, with its subfolders, to the set Luxforge organizes into events: typically `~/Pictures` or a card-dump folder. It is a short list the person controls, remembered with the catalog; removing a folder from it forgets nothing but its cache.

Whatever is browsed is **indexed**: a bounded, cancellable walk that lists supported files and reads each one's header — capture time to the subsecond with its offset, GPS position when present, camera make, model and body serial, lens, exposure time, f-number, ISO, exposure bias and focal length, dimensions and orientation — and no image data. It follows no symbolic link out of its folders, crosses no volume boundary and refuses past its file limit with `resource-limit`. Files are identified by path and signature (length, modification time, file identity), with no fingerprint: hashing a thousand RAW files would cost more time than culling them. A preview lane then extracts each file's embedded previews, the frames on screen first. Returning to a folder re-lists it cheaply and reconciles by signature. A folder on an unmounted volume is **offline**: its cached previews stay browsable; developing waits for its files.

### Events

Events are Luxforge's organization over the indexed folders and mounted cards ([P3](#proposals)). They are computed, not stored (`organize::events`): a pure function of the files' header metadata, the gazetteer and the view's thresholds, so a changed threshold regroups at once.

- **Where one ends.** Photographs are sorted by capture time across every camera and folder. A new event starts:
  - at a **jump**: a photograph whose position is more than 25 km from the last positioned photograph of the current event. The last positioned one, not the one just before: a camera without GPS interleaves with a phone that has one, and must not hide the phone's move;
  - at a **gap** of more than 3 hours between consecutive photographs, unless the gap is a **stay**: under 24 hours, with the last positioned photograph before it and the first positioned photograph after it (before the next gap of more than 3 hours) within 25 km of each other. A trip stays one event across its nights ("Konstanz · 12–13 Sep"), and a long break at one place does not split a day. Without a position on both sides, each outing is its own event, because nothing says it did not move. A gap of 24 hours or more always starts a new event; the 24 hours are fixed, not a threshold.

  The known limit: a place photographed every day with positions, such as home, stays one event for as long as no day is skipped, since nothing tells home from a trip. A second limit comes from the clocks: photographs are ordered by their true instant when the camera records its offset from UTC, and by its wall clock read as UTC when it does not, so on one trip a camera that writes no offset (many Leica, Fujifilm and DJI bodies) is placed later or earlier than one that does by the time zone's offset. The 3-hour gap absorbs this in Europe; further from UTC the two cameras can fall into separate events. Proposal, not built: order events by each camera's wall clock, which travellers set to local time, instead of the instant.
- **Names** ([P4](#proposals)), from what the event holds, in this order:
  - **Its place**: the median of its positioned photographs' positions (component-wise; across the antimeridian, western longitudes are taken past 180° for the median), named from an offline gazetteer bundled with Luxforge, never an online lookup, whose places are GeoNames' `cities15000` extract, compiled in and credited in the notices under CC BY 4.0 ([bundled place names](../engineering/dependencies.md#bundled-place-names)). The name is the most populous place within 10 km when it has at least three times the people of the nearest place that is not a district of another, else that nearest place when it is within 50 km, else none. So the centre of Berlin is "Berlin", not "Mitte", and the Louvre is in "Paris", not "Paris 01 Louvre", while a town beside a bigger neighbour keeps its own name: Lindau beside Bregenz, Bregenz beside Dornbirn, Venice beside Mestre, Versailles beside Saint-Quentin-en-Yvelines, each of which the most populous place within 10 km alone misnames. A place with fewer than 15,000 people is not in the table: Reichenau is named "Radolfzell" (9 km), Zermatt "Sierre" (34 km), and Hallstatt nothing.
  - **A user-named folder** holding more than half of its photographs: not a camera's folder (`DCIM`, or a DCF name of three digits from 100 and five characters, such as `100NZ8_1`, `100MSDCF` or `101_FUJI`) and not only a date (digits and `-_. `). A leading date of four digits or more is dropped when a name remains: "2026-09-14 Lake" names "Lake · 14 Sep", while "100 Days" stays whole.
  - **Its dates and cameras**: "16 Sep · LEICA Q3, Apple iPhone 15 Pro", the cameras by frame count, two at most and then how many more ("NIKON Z 8, LEICA Q3 +1"); the dates alone when no photograph records its camera.

  Dates are camera-local days in English: "12 Sep", "12–13 Sep", "30 Sep – 2 Oct", with the years only when the event crosses one ("30 Dec 2025 – 2 Jan 2026"). The first query builds the gazetteer's index, under 10 ms in a release build; every later one takes about a microsecond, so the index is warmed from a worker, never on the catalog owner thread.
- Events are listed by month, newest first, each with its picks and total, and are searchable by place, date and camera from the sources panel.
- An event's photographs from several folders and cards are one view; the Info panel says where each file is.
- An event is identified by its first file's path and capture instant, so it keeps its identity while later files join it.

A photograph with no capture time joins an "Undated" event per folder ("Undated · From Anna"), sorted by file name and listed after every dated event, and identified by its folder.

**Corpus evaluation.** Generated data only (`cargo xtask generate-catalog`), recorded with the default thresholds:

- **Image folders** (`--images` 120, 600, 3,000 and 5,000; seeds 3, 1, 7 and 11): every file's header read through the index's header reader, organized and compared with the generator's manifest. All six events of each run are found with exactly their files, their places and their names: "Konstanz · 12–13 Sep" (two days, both Nikon bodies, the Leica and the phone), "Lake · 14 Sep" (no GPS at all, named by its folder), "Zürich · 16 Sep" and "Luzern · 16 Sep" (41 km apart within the hour, split by the jump), "Lindau · 18–20 Sep" (three days, with the drone) and "Undated · From Anna". The generator bridges each trip's nights with a tripod's frames under 3 hours apart; with those frames removed, 11 nights stayed inside their trips and the one night with no positioned frame on one side split, as the rule says.
- **Index rows** (`--files` 10,000 with seeds 1 and 5, and 50,000 with seed 2): events, days, cameras and moments equal the independent reference grouping of the tests, and each older trip's folder (29, 29 and 185 trips) is exactly one event holding nothing else.
- **Names** over the generator's 34 places and a dozen city districts, suburbs and border towns: the rule above names each place as expected except the places too small for the table (above). The most populous place within 10 km alone misnamed Lindau, Bregenz, Venice and Versailles; a same-country restriction misnamed Bregenz, Venice and Versailles. Kreuzlingen, 1 km from Konstanz across the border and a quarter its size, is named "Konstanz".
- **Misgrouping**: none on this data. A labelled corpus of real trips from several makes does not exist yet, and its evaluation is outstanding: the acceptance's corpus check is not claimed.

### Grouping inside a view

A view of files is shown **Day › Camera › Moment** by default ([event board](catalog/event.png)): the camera's local capture day, then the body (make, model and serial) when a day has more than one, then moments of consecutive frames of one body ([P5](#proposals)). Grouping is computed like events (`organize::group`) and changes with its thresholds at once. Undated files are one group after the days, with no camera groups or moments: they are ordered by folder and name.

1. **Close in time.** Frames of one body on one day less than 1 second apart are one run; frames less than 2 seconds apart whose aperture, ISO and focal length match are also one run, since bracketing at slower shutter speeds spaces frames further. Many cameras record capture time to the second only (no `SubSecTimeOriginal`: most DNGs, Olympus ORF); when both frames' times are whole seconds, a gap equal to the threshold joins too, so a 10 fps burst that crosses a second boundary, which such a clock shows as exactly 1,000 ms, stays one run.
2. **Burst or bracket.** A run whose exposure steps is a **bracket**; otherwise it is a **burst**.
   - **From metadata.** Exposure time, f-number and ISO give each frame's exposure value; when they are the same on every frame, the recorded exposure bias does, as some drones and phones write it. The two are never added, since a camera applies its bias through its settings. A run of 2 to 9 frames whose values are all distinct, each at least ⅓ EV from the next once sorted, is a bracket, labelled "from metadata". The ⅓ EV allows 0.13 EV for nominal values: cameras record third stops rounded, so 1/200 → 1/250 s records 0.32 EV, 1/50 → 1/60 s and f/3.2 → f/3.5 0.26 EV, and 1/13 → 1/15 s 0.21 EV, each a true ⅓ EV; a sixth of a stop (0.17 EV) does not count. A longer run, or one whose values repeat, that repeats one bracket's pattern (every frame within 0.13 EV of the frame a period later, and each period a bracket) is that many brackets, as an HDR panorama shoots them one after another.
   - **From previews.** When the metadata cannot say — no bias recorded and nothing else changing — organizing asks the preview check (lane B's, not built yet: until it is, such a run is a burst) about a run of 2 to 9 frames. It compares the frames' grid previews: the mean log luminance of a small central-weighted thumbnail of each, with the framing checked unchanged by a correlation of their downscaled edges. Distinct measured steps at least about ⅔ EV apart (⅔ EV less ⅙ EV, since a camera's tone curve and clipping compress a bracket's outer steps) make a bracket, labelled "from previews" with the measured step ([components board](catalog/components.png)). The comparison costs microseconds on previews already decoded for the grid.
   - A bracket's steps are relative to its metered frame: the one frame with no bias from metadata, else the median frame ("−2 · 0 · +2 EV").
3. Anything else is a single frame.

Thresholds are adjustable from the Group chip and apply at once; they are a view setting, not stored corrections. The Group chip also switches to Day only, or none. Panoramas and visually similar frames are later groupings ([scope](#scope)).

In the grid a moment is a framed row with a header naming its kind, span or exposures and how many are picked ("Burst · 6 frames in 1.4 s · 1 picked", "Bracket · 3 exposures · −2 · 0 · +2 EV · from metadata"). A burst collapses to one cell showing its pick, or its first frame, with the frame count (`S`).

**Corpus evaluation.** Generated data only, with the default thresholds. Over the four image folders above, every burst and bracket of the manifest is found with exactly its frames, kind, evidence and steps, and no other moment: 307 brackets from metadata (with the bias recorded, as the Nikons and the Fujifilm write it, and without, as the Leica does), 144 brackets from previews through a stand-in probe answering from the manifest (the drone's, whose metadata does not change), and 781 bursts; with no probe the 144 are bursts. The generator's moments are at least 6 seconds apart, so the tests' reference comparison covers the edges: 4,000 random frame sets from fixed seeds (about 440,000 frames), with gaps at every threshold, whole-second clocks, drifting and repeated exposures, nominal third-stop brackets, consecutive brackets and preview-only brackets, group exactly as an independent plain implementation. A labelled corpus of real bursts and brackets from several makes does not exist yet, and its evaluation is outstanding.

### Browsing at speed

- **Grid.** Virtualized: only visible rows and a margin exist. Each cell is the camera's embedded preview at grid size, drawn soft from the file's EXIF thumbnail until that preview is read; a picked frame carries the accent check; a file already in the catalog says so. States are on the [components board](catalog/components.png).
- **Loupe** (`Space` or `E`, [board](catalog/choosing-from-a-burst.png)) shows the active frame fitted to the screen, from the largest embedded preview, with a bar naming the moment, the frame's position and time offset, its exposure and "Camera preview" with the preview's size. Under it, the moment's frames, numbered. `←` `→` move through frames, `↑` `↓` between moments, `1`–`9` jump to a frame. `Tab` hides the side panels, as in Develop.
- **100% focus check** (`Z`) shows the region under the pointer at 100% in an inset, from the embedded preview when it is full size, decoded only for that region. When a camera's preview is smaller than the sensor, the inset is a neutral Luxforge development of that frame, made on demand off the editor's source cache and labelled as such ([P6](#proposals)). The core's region decode and development fallback are built ([the 100% region](#the-100-region)); `preview.region` and the inset are not yet wired to them.
- **Compare** (`C`) shows up to four frames of the moment side by side at the same zoom.
- **Look-ahead.** The loupe decodes the next frames in the direction you are moving, and the next moment's first frame, within a byte budget.

### Keeping up with the disk

Organizing files is realistic because what it needs is small and near the start of each file. The expensive part is previews, so the design keeps the two apart.

- **What is cheap.** Events and moments need only header metadata, usually within a file's first 64 KB: well under a second for 1,000 files on the internal SSD, a few seconds from an SD card. Sorting, grouping and naming even 100,000 rows takes milliseconds. Most RAW files also carry a tiny thumbnail (about 160 px) beside their metadata, so the grid can draw every cell at once, soft, from the same reads, and the bracket brightness check runs on it.
- **What is not.** A sharp grid cell or a loupe frame needs the camera's larger embedded preview, 1–3 MB a file. From a UHS-I card at about 90 MB/s that is 20–30 s for 1,000 files, about 8 s from the M4 Pro's UHS-II slot. So previews are read in priority order — the loupe's look-ahead, the visible cells, then the rest — and never block browsing. A camera whose files carry no usable preview (some DNGs among them) needs a real development of about 0.3–1 s a file, done lazily for what is on screen and reported per camera as the preview lane is built.
- **How it stays current.** Indexed folders are watched with the platform's change notifications, never polled: FSEvents on macOS, which also replays what changed while Luxforge was closed; the change journal or directory notifications on Windows; inotify on Linux. A card cannot be watched usefully — the camera writes to it elsewhere — so it is re-listed when it mounts, and reconciled by signature (length, modification time, file identity): unchanged files keep their rows and previews, changed ones are read again, new ones are added and vanished ones dropped. 1,000 files reconcile in well under a second. A network volume sends no reliable notifications, so it is re-listed when opened. Nothing wakes while nothing changes ([performance rule 8](../engineering/performance-rules.md#rules)).
- **Moves.** A rename or move within a volume keeps its file identity, so the index carries picks across it, and a developed photograph whose file moved within its volume is found again by identity and confirmed by fingerprint without anyone asking.
- **Events may reshape.** Events are computed, so files added to a folder can merge or split them and change a name. Nothing is lost: picks follow files, and catalog folders, once made, are the person's.
- **What indexing skips.** macOS packages (such as a Photos library), other applications' caches and previews (such as Lightroom's `.lrdata`), hidden folders, system folders and Luxforge's own catalog and index directories.
- **Clocks.** Camera clocks drift, lack a time zone or were never set. Moments are per body, so they do not care; events use a 3-hour gap, which absorbs small drift between bodies; a camera whose clock is badly wrong forms its own event, which the person can see and ignore. Correcting capture times is later work.
- **Scale.** The first index of a large folder — `~/Pictures` with 200,000 files — reads headers for a minute or two and runs in the background (below). Later visits cost only the changes.

### Long-running work

Luxforge always shows that intense work is happening, how far it has got and how to stop it ([P18](#proposals), [components board](catalog/components.png)).

- **Background by default.** Every job expected to take more than a second — indexing, reading previews, developing picks, searching for missing originals, batch preset and export — is a row in the Performance section with its progress, an estimate once one is truthful, and **Cancel**, through the one activity board and the progress model every activity already shares (`activity.list`; `job.cancel`). The section's cancel is the follow-up the [performance panel design](performance-panel.md) already lists. While any job runs, the status bar carries the busiest one with a small bar and the number of jobs; clicking it opens the Performance section. A finished job leaves a sentence in the status bar ("Indexed 12,408 files in ~/Pictures").
- **In front only when there is nothing to show.** When the view you are in cannot show anything yet — the first look at a card or folder before its file list and headers are read — its progress is a sheet in that view ("Reading the NIKON Z 8 card · 312 of 612 files · about 4 s left") with **Continue in background** and **Cancel**. It covers only the view that is waiting, never the rest of the window, and it goes away by itself when the first frames can be drawn. Nothing else is modal.
- **Honest numbers.** A count is shown when the work knows its extent ("48,210 of about 200,000 files" while a walk is still discovering it), an estimate only once the rate is steady, and "working" otherwise; a stuck job is never shown as nearly done.

### Picking

A file is picked or not (`P` toggles; on a selection it picks or clears all). There are no stars, keywords, flags or rejects: selecting has one decision. Picks are kept by path and signature with their actor and time until they are developed or cleared, so a culling session survives a restart; a picked file whose signature changes keeps its pick and loses its cached previews. In the loupe, picking a frame of a burst moves on to the next moment ([P7](#proposals)), because one frame of a burst is usually the one; the others stay as they were. **Moments without a pick** narrows the grid to what is left to decide. Every pick and clear is a library change with undo.

### Developing picks

**Develop N** in the title bar (`Cmd+Return`) brings every pick in the current view into the catalog, then opens Develop on the first with the filmstrip holding them. It first asks, in a small confirmation under the button ([event board](catalog/event.png)), which **catalog folder** they go into ([P15](#proposals)):

- The default is a new folder named after the picks' event ("Konstanz · Sep 2026"), selected for typing, so Return accepts it and typing names it. **Or add to an existing folder** chooses another.
- Picks from an event that already has a catalog folder — you come back next month for another frame — default to that folder.
- Picks from several events default to one folder per event, listed in the confirmation with their counts.
- The confirmation also says what is about to happen to picks on a card (below). Escape cancels and nothing changes.

Then: `D` on a frame that is not picked picks it and does the same. Bringing a file in is a job on its own lane: for each file a bounded read that streams its fingerprint (the SHA-256 the catalog already uses), reads its RAW interpretation without developing it, and creates the asset with its Original entry and its capture metadata. A cancelled or failed Develop keeps what it committed and nothing partial; the picks it committed are cleared, the rest stay.

- A file whose bytes are identical to a photograph already in the catalog is linked to that photograph rather than added twice, and the grid marks it "In the catalog". A file whose name and size match a photograph whose original is missing, and whose fingerprint matches, relinks that photograph instead ([source recovery](../specs/source-recovery.md)).
- **Brackets.** Developing a bracket's picked frames brings each in as its own photograph, linked by the moment they came from. **Pick all 3** in the moment header picks them together. One photograph merged from the bracket is a later merge module's job ([HDR and other merges](#hdr-and-other-merges)).
- **Removable media.** Before developing files that are on a card or other removable volume, Luxforge says so. If the same bytes are already in an indexed folder — the card was copied — it offers to use those files instead, matched by name and size and confirmed by fingerprint. Otherwise the person confirms, and the catalog points at the card ([P8](#proposals)).
- **Changing your mind.** A developed photograph with no edits can be sent back: its catalog record, which holds only its Original entry, is deleted and the file is picked again. An edited one leaves the catalog only through Remove ([P9](#proposals)).

A single file opened directly (a drop on Develop, or `luxforge --open FILE`) is picked and developed at once, which is today's single-photograph behaviour.

### Develop and the development set

Develop's filmstrip ([board](catalog/development-set.png)) is a 92 pt strip under the canvas, between the side panels, holding the **development set**: the photographs Develop was opened with — what was just developed, Recently developed, a collection or a catalog search. Its header names the set ("Development set"). `←` `→` move through it. Moving shows the photograph's cached large preview at once, labelled a preview in the status bar's render slot, while its original is prepared through the existing source path; the controls enable when it is ready and the exact render replaces the preview, as it does today. The filmstrip collapses with `Cmd+Option+F`.

### The catalog

The catalog's photographs are browsed in Select through the **Catalog** sources ([board](catalog/catalog.png)): All photographs, Recently developed, **catalog folders** grouped by year, collections, Missing originals and Removed. Over the catalog the filter bar holds a search field (`Cmd+F`: file names, places, camera, lens), chips for Kind and Edited, and **Metadata**, which opens a browser of four columns (Date, Place, Camera, Lens) with counts, each value narrowing the view. **Save as smart collection…** saves the source, filter and sort. Sort is Capture time (default, newest first), Date developed, File name or Last edited.

The catalog is organized by **catalog folders**. Every developed photograph is in exactly one, the one it was developed into. The sources panel lists them under their year (the year of a folder's earliest photograph, a grouping only), newest first. A folder can be renamed, nested in another folder, merged into another, or deleted when it is empty; photographs move between folders by dragging or **Move to…**; `+` in the Catalog header makes an empty folder. Nothing on disk changes when catalog folders do ([P15](#proposals)).

Across folders there are **collections**, sets a photograph can be in any number of (a portfolio, a print order), and **smart collections**, saved searches that may not refer to another smart collection. There are no ratings, keywords or flags ([P2](#proposals)): what is in the catalog was chosen when it was picked, and folders, collections, places and dates find it. With several photographs selected, the Info panel moves them to a folder or adds them to or removes them from collections as one library change, and offers **Apply preset…** (each photograph its own history entry, `Preset: <name>`, with any it could not apply to listed, never skipped silently) and **Export…** (the existing JPEG export of each current entry into one folder, by the existing naming rule, [P11](#proposals)).

### Library changes and undo

Every pick and clear, folder and collection change, Develop and relink is one **library change**: a numbered row naming the actor, request, method and a label ("Picked L1003206.DNG", "Added 5 to Portfolio › Landscapes", "Developed 18"), with each item's value before and after. The journal is append-only. `library.undo` reverts the calling client's latest change not yet undone by appending its inverse, and is refused, naming the items, when any of them changed since; `library.redo` reverts that undo under the same rule. In Select, `Cmd+Z` and `Shift+Cmd+Z` are these; in Develop they stay the photograph's history ([P10](#proposals)). A Develop is undone by sending its unedited photographs back; one already edited refuses the undo with that reason.

How it works (built for picks; every later library change records through the same core, `crate::library::journal`):

- A method says only what each item is to become. In one catalog transaction the journal reads each item's value, leaves out the items that already have it, writes the rest and records each with its value before and after; a change that changes nothing records nothing and announces nothing. One reader and one writer per item kind (`library::items`) serve every change, its undo and its redo, so undo and redo revert any change the same way: an undo writes each row's `before` back in reverse order, and records that as a change that `undoes` the first.
- The calling client is the request's `actor` (`library_changes.client_key`), so an undo survives a restart. Undo reverts the actor's latest change or redo not yet undone; redo reverts its latest undo not yet redone, made since its latest new change, which ends what can be redone as in an editor.
- An item "changed since" when a later change that is still in effect touched it — a later change and its undo, or a later undo and its redo, cancel out — or when its value is no longer the one the change left, whoever changed it. The refusal is `conflict` with the items in `data.items` (the first 100, and `data.count`); nothing changes.
- A retried request, the same actor, request identity and method, is answered with the change its first attempt recorded, after a restart too, before its targets are resolved again.

One change covers at most `MAX_LIBRARY_BATCH` items (provisionally 50,000, set by measuring the owner transaction); a larger request is refused with `resource-limit`, never split silently.

### Missing originals

Catalog folders are the catalog's own organization and need not mirror the disk, so recovering originals works per photograph, not per catalog folder. Each developed photograph records the **folder on disk it was developed from** (`source_folder`), which is what lets many photographs be resolved at once.

Each developed photograph also records its original's last observed availability (available, offline, missing or changed) and when it was checked. An absent mount point makes all its photographs offline with one check; checks run for the visible view first and the rest in the background, never outside known folders. An offline or missing photograph stays browsable and organizable with its cached previews; Develop and export need the original and offer **Locate original…** in the existing notice.

**Missing originals** in the Catalog sources ([board](catalog/resolve-missing.png), [P11](#proposals)) lists every photograph whose original is not where it was, **grouped by the folder on disk it was developed from**, each group saying how many photographs it holds, which catalog folders they are now in, and why they are missing ("Photos SSD is not connected", "the folder is gone from Macintosh HD"). A group whose volume is merely unplugged needs nothing but the drive; connecting it resolves the group.

- **Find in a folder…** on a group searches one chosen folder, with its subfolders, for each of the group's files, bounded and cancellable: first by file name and size, then by fingerprint for every candidate. Each photograph gets its own result: **found, same bytes** (with the new path); **different bytes** at the same name, left as it was; **several files with the same bytes**, to choose between; **not found**; or still checking. A file that another photograph in the catalog already points at is reported, never taken.
- **Locate…** on one row picks one file for one photograph, fingerprint-checked, as the accepted [Locate](../specs/source-recovery.md#locate-missing-original-follow-up) does.
- **Relink N** commits every verified result at once, in one transaction, as one library change that can be undone. Filters narrow the list to Found, Needs you or Not found. Nothing changes until Relink; stopping a search or unplugging a drive leaves every photograph as it was.
- Relinking changes only where the catalog looks for a file. The photograph's catalog folder, collections, edits and history stay; its recorded source folder becomes the new one.

### Removing

"Remove from catalog…" (`Delete`, with a confirmation naming the count) moves photographs to **Removed**, hidden elsewhere, with their edits, history and collections kept and Put back available. Their files are untouched. **Empty Removed…** permanently deletes their catalog records, the only destructive catalog operation, and only the desktop or `luxforge-json --permission-authority` may run it ([P12](#proposals)).

### Keyboard

| Keys | Select | Develop |
| --- | --- | --- |
| `G`, `D` | Grid; develop the active frame (picking it if needed) | Select's grid; — |
| `Space` or `E`, `Esc` | Loupe; back to the grid | — |
| `←` `→`, `↑` `↓` | Frames; moments (loupe) or rows (grid); with `Shift`, extend the selection | Previous and next in the development set |
| `1`–`9` | Jump to that frame of the moment (loupe) | `1` stays 100% |
| `P` | Pick or clear | — |
| `Z`, `C`, `S` | 100% focus check; compare the moment; collapse or expand a burst | — |
| `Cmd+Return` | Develop the picks in view | — |
| `Cmd+O` | Add a folder… | Add a folder… (a single file lands in Develop) |
| `Cmd+A`, `Cmd+D`, `Cmd+F` | Select all in view, select none, search | — |
| `Cmd+Z`, `Shift+Cmd+Z` | Library undo and redo | The photograph's history |
| `Tab`, `Cmd+Option+F` | Side panels | Side panels, filmstrip |
| `Delete` | Remove from catalog… (catalog views only) | — |

Letters act only when no text field has focus, from the one keymap table (`app/keymap.rs`).

### HDR and other merges

Not built, and not proposed for the first version; this is the room the design leaves for it. A bracket's frames are already detected and, when all are developed, linked by their moment. A later **merge module** would add a third source kind beside JPEG and RAW: a photograph whose source is an immutable derived artifact — the merged, scene-linear result of named input photographs, their fingerprints and the merge's parameters — produced by a module task through the existing [module capabilities](module-capabilities.md) (tasks, resources and derived artifacts beside the catalog). The merged photograph has its own recipe and history; its inputs stay in the catalog; a missing or changed input makes its regeneration fail explicitly, never silently. In Select the bracket header would then offer **Pick as HDR** beside Pick all, which the [components board](catalog/components.png) draws as a later state. Panorama stitching would use the same source kind with a panorama moment. Merging stays unselected until the owner selects it ([decisions](../decisions.md#owner-workflow-priorities)).

## Screens

The boards are on the design canvas with the Develop boards, under "Select, then develop — the catalog (proposal)", at 1440 × 900 with the Develop workspace's tokens, widgets and density, rendered here at 2×. Thumbnails are CC0 sample files from raw.pixls.us across fourteen camera models, plus the owner's Sapa JPEG; the burst and bracket frames are made from single samples, and names, places, times, lenses, counts and the agent are illustrative.

| Board | Shows |
| --- | --- |
| [An event](catalog/event.png) | An auto-organized event across a card and a folder, grouped by day, camera and moment: a burst with its pick, a collapsed burst, a bracket from metadata with Pick all 3, the Info panel's pick, moment and place, and Develop 18's confirmation naming the new catalog folder |
| [Choosing from a burst](catalog/choosing-from-a-burst.png) | The loupe with panels hidden: one frame at Fit, the 100% focus check inset, the moment's numbered frames with the pick, and the keys |
| [Development set](catalog/development-set.png) | Develop with the workspace switch and the filmstrip holding only developed picks, showing a cached preview while the original prepares |
| [The catalog](catalog/catalog.png) | Developed photographs with catalog folders by year in the sources, the metadata browser (Date, Place, Camera, Lens), filter conditions, Save as smart collection and a five-photograph batch with its folders and collections |
| [Resolving missing originals](catalog/resolve-missing.png) | Missing originals grouped by the folder on disk they were developed from, one group being searched in a new location with a result per photograph, and Relink for the verified ones |
| [Components](catalog/components.png) | Cells, a burst, brackets from metadata and from previews with the later HDR state, the Develop button's states, cards and events, the removable-media warning, and long-running work: Performance rows, the status bar's job and the in-view progress sheet |

![An auto-organized event in Select](catalog/event.png)

Visual rules beyond the [Develop visual language](develop-workspace.md#visual-language): the photograph stays the only colour. The accent marks a pick (a filled check), the active frame's outline, a set filter condition, the pick counts and the one primary action, Develop N. Moment rows sit on a step between the canvas and the panel (`#202024`) with a hairline outline; a moment's evidence ("from metadata", "from previews") is a neutral tag. The clipping red is used only for unavailable originals and unreadable files.

## Model

### Storage

The catalog, format 12, adds:

| Table | Holds |
| --- | --- |
| `picks` | Per picked file: path, signature, actor, time and the request that picked it. Removed when the file is developed or the pick cleared |
| `indexed_folders` | The folders on disk the person added for events |
| `catalog_folders` | Catalog folders: name, parent, creation time and the event span they were made from, so later picks from the same event find them |
| `assets` (new columns) | `catalog_folder_id`, `source_folder` (the directory on disk), `volume_id`, `file_name`, `developed_ms`, `removed_ms`, `availability`, `checked_ms`, `develop_moment` (the burst or bracket it was developed from); its `row_id` is the stable integer key a view holds |
| `capture` | Per asset: capture time (UTC milliseconds and camera-local text with offset), position and place name when known, make, model, body serial, lens, exposure time, f-number, ISO, exposure bias, focal lengths |
| `volumes` | Mount point, label, whether removable, and the platform's volume identifier where one exists |
| `collections`, `collection_members` | Collections, smart collections (with their query) and groups |
| `library_changes`, `library_change_rows` | The journal, with each item's value before and after |

A photograph's position is kept because places are how the catalog is searched; it stays on the Mac, and export's metadata rules are unchanged (stripped by default, the design's field set when kept). As for every earlier format, a format-11 catalog is refused by name without being rewritten, and the owner starts a new catalog; there is no migration ([current shapes only](../../AGENTS.md#engineering-rules)).

### The index and previews cache

`<catalog>.index/` beside the catalog holds everything disposable, and can be deleted while Luxforge is closed at the cost of time only:

- `index.sqlite`: one row per file seen (path, signature, header metadata, where its embedded thumbnail sits), the roots listed, and the preview records of files and developed photographs. It is its own database with its own format marker, opened on first use, so rebuilding it never touches the catalog's; a mismatched format, a database SQLite cannot read, or another catalog's index is discarded and recreated. Events and moments are computed from it on demand and cached in memory.
- `previews/`: JPEG previews. **For a file**, keyed by signature: the **grid** tier from its EXIF thumbnail or embedded preview, and the **loupe** tier (the largest embedded preview, stored at up to 2560 px on the long edge). The 100% check reads the original's embedded full-size preview for the region it needs, or develops the frame and keeps that one development in memory ([below](#the-100-region)), and caches nothing here. **For a developed photograph**, keyed by asset, entry and tier: **grid** (512 px) and **large** (2048 px, for Develop's instant switch), rendered from the current entry through the proxy path; until the first render a RAW shows its camera preview, marked as such.

Grid tiers are kept (about 40 KB each); loupe and large tiers share a byte budget with least-recently-used eviction ([P8](#proposals)). A changed renderer generation discards the rendered tiers.

**Built for files** (`core/src/previews/`, `api/owner/previews.rs`):

- **Tiers.** Every tier is a baseline JPEG, upright (the file's EXIF orientation applied, so a client draws it as it is), fitted within its long edge and never enlarged, with the file's ICC profile kept. The **grid** tier, at most 512 px, arrives in two stages: the file's thumbnail (origin `exif-thumbnail`, which `browse.rows` reports as `thumbnail`), then its embedded preview (`embedded`, reported `ready`), which replaces it. For a JPEG original the thumbnail is the EXIF thumbnail the index row locates, read at its offset and length, and the embedded stage and the loupe tier are the original itself, decoded at the DCT scale that covers the tier and box-downscaled with the proxy's own downscale. For a RAW one `EmbeddedPreviews` open serves both stages: the thumbnail is the smallest extractable image (a JPEG, or on most Nikon bodies a 160 × 120 bitmap) when the file carries another, and every other tier comes from the largest JPEG by stored length, then the other JPEGs, then the largest bitmap. A camera JPEG with bytes after its last EOI (every Canon CR3's full-size preview, the Leica CL's, Q2's and SL2's, some DJI files') is cut at that EOI before the strict codec decodes it. The **loupe** tier is the largest embedded preview at most 2560 px: a DJI's 960 px preview stays 960 px.
- **Validity and names.** A tier records the signature of the file it was made from and is served only while that equals the index row's signature; a stale row, or one whose file is missing or empty, is never served and is removed, row first, by the next task that makes that tier. Each file is named from what made it, `previews/files/<xx>/<file>-<tier>-<origin>-<signature hash>.jpg`, so a replacement never overwrites a path a client may be reading, and `key` names the file's row, signature hash, tier and origin.
- **Writes.** A worker writes the JPEG to a temporary file beside its name, renames it into place, then writes its row, then removes the file the row named before. Nothing is flushed: the cache is disposable.
- **Budget.** Loupe tiers and developed photographs' large tiers (`photo_previews`) share 4 GiB (`SHARED_PREVIEW_BUDGET_BYTES`), a parameter of the lane: after a worker writes a loupe tier it removes the least recently used of either until they fit, never the one just written. A served loupe tier records its use at most once a minute. Grid tiers are never removed.
- **Unusable files.** A file with no usable preview (only H.265, as on the Canon EOS R5 Mark II and R8, or nothing that extracts and decodes) fails `unsupported-input` naming why, through the one seam where its development will go (`develop_instead` in `previews/extract.rs`). The lane remembers the failure in memory, per tier and signature, up to 20,000 of them, never in the index: `preview.read` answers it again at once, the grid reports `unavailable`, and the file is read again once it changes or Luxforge restarts.
- **Reads for the other lanes.** `previews::grid_states` (one query over a list of files, for `browse.rows`; the owner's lane adds `unavailable`) and `previews::cache_bytes` (the cache's bytes by tier and its file count, for `catalog.info`).

Not built yet: the lane's use of the development fallback for files with no usable preview, and `preview.region` on the lane (their domain functions are built: [the 100% region](#the-100-region)), and developed photographs' rendered tiers (TASK-010).

**What cameras embed** ([inventory](../research/embedded-previews.md), 120 files from 103 cameras). `luxforge-raw` lists a RAW file's embedded images and extracts one by positional reads, without unpacking it ([its README](../../crates/luxforge-raw/README.md#embedded-previews)); listing read 17 to 205 KB a file. Which tier needs the development fallback follows from the camera:

| Cameras | Largest embedded preview | Needs a development for |
| --- | --- | --- |
| 53: every Nikon, Pentax, Ricoh and Leica; every Canon but the EOS R5 Mark II and R8; Sony A1, A7 IV, A7C II, A7CR, A7R V, A7S III and a6700 | Full-size JPEG | Nothing: the 100% check reads the preview |
| 43: every Fujifilm, OM System, Olympus and Panasonic; Sony A7 III, A7R II–IV, A7C, A9 and a6000–a6600 | JPEG of 1616–4416 px | The 100% check |
| 5: the DJI drones, the owner's Air 2S among them | JPEG of 960 px | The loupe and the 100% check |
| 2: Canon EOS R5 Mark II and R8 | H.265 only, which is not extracted | Every tier, the grid included |

#### The 100% region

The core's region functions (`previews/region.rs`) answer one rectangle of a frame at full resolution. They run on a preview-lane worker, never on the catalog owner, and read the file's current signature first: a file that changed since it was indexed, or is gone, is `source-unavailable`.

- **The frame.** A rectangle is in the image's upright full-resolution pixels: a JPEG original's frame after its EXIF orientation, and for a RAW the upright size of Luxforge's own development, which the fallback shows and a full-size embedded JPEG approximates (0.99 to 1.00 of it). A caller that knows the rectangle against another upright frame, such as the header's dimensions or the loupe's preview, names that frame; each path then maps the rectangle's centre proportionally into its own source's frame and keeps its size at 1:1, since a 100% region is never resampled. The rectangle is clamped to the source, and the answer reports the rectangle it returns in the frame it cut it from. A rectangle of no pixels, one starting outside its frame or one past 32 MiB of pixels is refused.
- **From the embedded preview.** A JPEG original is its own full-size image. For a RAW, the largest embedded JPEG is full size when both its edges are at least 95% of the visible image's, long edge against long edge (the inventory's rule); a listed size that already fails it saves extracting the preview. The JPEG is cut after its last EOI marker (Canon CR3, Leica and some DJI previews carry bytes after it), the rectangle is mapped to the stored preview through the inverse of the file's orientation, `luxforge-jpeg`'s region decode reads only the rows and columns it needs, and the pixels are turned upright: byte for byte the same rectangle of the preview's whole upright decode, allocating the rectangle and one strip of it. The pixels are shown as stored, as the loupe shows the preview, with no ICC profile applied. Labelled `embedded`.
- **From a development.** Where the preview is not full size, or the file carries none, the frame is developed as the editor shows an unedited RAW: the Original's rendering, its RAW development at the camera's as-shot white balance, read through one bounded read of the file, decoded, developed and rendered on the caller's thread without the editor's source cache or source worker. One development runs at a time in the process; at most four callers wait for it, each cancellable while it waits, and a fifth is `resource-limit`. The last development is kept, keyed by the file's path and signature, as upright display bytes (within the 512 MiB frame limit), so the next region of the same frame is a copy of its rows and a frame the slot keeps answers before the embedded preview is listed again; it is released before another frame is developed. The same development makes a downscaled tier, up to the loupe's 2560 px, for a RAW with no usable preview. Labelled `developed`. A camera outside Luxforge's RAW catalog is `unsupported-input`.
- **The answer's JPEG** is written at quality 95 with 4:4:4 chroma, so fine detail is not averaged away.

Not yet wired: the `preview.region` job on the preview lane, writing its answer file (valid until the client's next region), waking waiting developments when a job is cancelled, releasing the kept development when Select closes, and the developed grid and loupe tiers of the Canon EOS R5 Mark II and R8.

## API

The methods, in the one method table with declared parameters. Every shape is declared once in the core's `catalog_types` module: each method's parameter struct, answer, envelope, job and error codes in `catalog_types/api.rs` (`CATALOG_METHODS`), and the types they name beside their concept (`identity`, `header`, `disk`, `organize`, `browse`, `library`, `previews`, `jobs`). Each lane adds its methods to the method table when they work (so far `event.list`, the `browse.*` methods, `pick.set`, `pick.list`, the `library.*` methods and `catalog.info`), so `schema.list` never lists a method that does nothing, and a test holds every registered catalog method to its declaration. Mutations carry `{request_id, actor}` (`mutation`) and the owner answers their retries; only `catalog.empty-removed` needs permission authority. A method that starts a job answers `{job_id, status, deduplicated}` at once, and the job's result, read with `job.read` and cancelled with `job.cancel`, is the answer named here. Every method may also answer `protocol` and `internal`.

`targets` is `{kind: paths, paths}`, `{kind: files, file_ids}`, `{kind: assets, asset_ids}` or `{kind: selection}` (the caller's selection in its current view), at most `MAX_LIBRARY_BATCH` (50,000) items; a larger request is `resource-limit`. A file is named by its index row (`file_id`, valid while the index exists) and always also by its `path`; a photograph by its `asset_id`.

| Method | Takes | Answers | Errors | Lane |
| --- | --- | --- | --- | --- |
| `index.add-folder` | `path`, `mutation` | `IndexFolderAnswer {change, folder, job_id?}`: a library change, and the `index.refresh` job that lists it | validation, read-error, resource-limit, conflict, catalog | A |
| `index.remove-folder` | `path`, `mutation` | `LibraryAnswer {outcome, change?, items, deduplicated}` | validation, conflict, catalog | A |
| `index.folders` | — | `IndexFolders {folders: [{path, volume_id, added_ms, actor, offline, files?, listed_ms?}]}` | catalog | A |
| `card.list` | — | `Cards {cards: [{volume, dcim, files?, cameras, events?}]}` | catalog | A |
| `index.refresh` | `source`: `{kind: indexed-folder, path}`, `{kind: card, volume_id}`, `{kind: folder, path}` or `{kind: all-indexed}` | job `index-refresh` → `IndexReport {roots, files, added, changed, moved, removed, unreadable}` | validation, source-unavailable, read-error, resource-limit, cancelled | A |
| `event.list` | `month?` (`YYYY-MM`), `query?` | `EventList {events: [Event {id, name, place?, first_day?, last_day?, months, cameras, count, picked, offline, roots, volumes, undated}], months: [{month, events, files, picked}]}`, newest first | validation, catalog | D |
| `browse.view` | `source`, `filter?`, `sort?`, `grouping?`, `thresholds?` (a `ViewQuery`) | `ViewSummary {revision, query, count, picked, in_catalog, unavailable, groups: {days, cameras, moments}, library_sequence, index_revision}`: evaluates the query into the caller's one view, held by the owner | validation, resource-limit, catalog | D |
| `browse.rows` | `from`, `count` (1–1000), `revision?` | `ViewRows {revision, from, rows: [ViewRow {position, item: file {file_id} or photo {asset_id}, path, file_name, kind, dimensions?, orientation?, capture?, place?, camera?, lens?, exposure, moment?: {index, frame}, picked, developed_as?, edited, availability, preview}]}` | validation, conflict | D |
| `browse.facets` | `source`, `filter?`, `facets`: `[date, place, camera, lens, kind, pick]` | `Facets {counts: {facet: [{value?, label?, count}]}}`: each count the size of the view that value gives | validation, resource-limit, catalog | D |
| `browse.select` | `mode?` (replace, add, remove, toggle), `items?`, `range?: {start, len}`, `all?`, `active?`, `revision?` | the session: `session.state`'s `browse {query?, revision, count, stale, selection: {count, ranges, active?}}` | validation, conflict | D |
| `pick.set` | `targets`, `picked`, `mutation` | `LibraryAnswer` | validation, resource-limit, conflict, catalog | C |
| `pick.list` | `source?`, `after?`, `limit?` | `PickPage {picks: [Pick {path, signature, volume_id, actor, request_id, picked_ms, file_id?}], next_after?}` | validation, catalog | C |
| `pick.plan` | `targets?` | `DevelopPlan {events: [{event_id?, name, count, folder: {kind: existing, folder_id} or {kind: new, name, parent_id?}, folder_name?, removable: [{volume_id, label, count, with_copy}]}], count, offline}` | validation, catalog | C |
| `pick.develop` | `into: [{event_id?, folder}]`, `mutation`, `targets?`, `use_copies?`, `confirm_removable?` | job `develop-picks` → `DevelopReport {developed: [{path, used?, asset_id, outcome: created, linked or relinked}], failed, changes}`; replaces `catalog.import` | validation, source-unavailable, resource-limit, conflict, catalog, cancelled | C |
| `folder.list` | — | `CatalogFolders {folders: [{id, name, parent_id?, created_ms, event?: {start_ms, end_ms, event_id?}, count, year?}]}` | catalog | C |
| `folder.create` | `name`, `mutation`, `parent_id?` | `FolderAnswer {change, folder}` | validation, conflict, catalog | C |
| `folder.rename` | `folder_id`, `name`, `mutation` | `LibraryAnswer` | validation, conflict, catalog | C |
| `folder.move` | `folder_id`, `mutation`, `parent_id?` (absent: the top level) | `LibraryAnswer` | validation, conflict, catalog | C |
| `folder.merge` | `folder_id`, `into_id`, `mutation` | `LibraryAnswer` | validation, conflict, resource-limit, catalog | C |
| `folder.delete` | `folder_id`, `mutation` | `LibraryAnswer`; conflict when the folder holds photographs or folders | validation, conflict, catalog | C |
| `asset.move` | `targets`, `folder_id`, `mutation` | `LibraryAnswer` | validation, resource-limit, catalog | C |
| `asset.send-back` | `targets`, `mutation` | `LibraryAnswer`: deletes unedited photographs' records and picks their files again; conflict for one with history beyond its Original | validation, conflict, resource-limit, catalog | C |
| `collection.list` | — | `Collections {collections: [{id, name, parent_id?, kind: collection, smart or group, query?, created_ms, count?}]}` | catalog | C |
| `collection.create` | `name`, `kind` (collection or group), `mutation`, `parent_id?` (a group) | `CollectionAnswer {change, collection}` | validation, conflict, catalog | C |
| `collection.create-smart` | `name`, `query` (a `ViewQuery` over photographs, never naming a smart collection), `mutation`, `parent_id?` | `CollectionAnswer` | validation, conflict, catalog | C |
| `collection.update-smart` | `collection_id`, `query`, `mutation` | `LibraryAnswer` | validation, catalog | C |
| `collection.rename` | `collection_id`, `name`, `mutation` | `LibraryAnswer` | validation, conflict, catalog | C |
| `collection.move` | `collection_id`, `mutation`, `parent_id?` | `LibraryAnswer` | validation, conflict, catalog | C |
| `collection.delete` | `collection_id`, `mutation` | `LibraryAnswer`: a collection with its memberships, or an empty group | validation, conflict, catalog | C |
| `collection.add`, `collection.remove` | `collection_id`, `targets`, `mutation` | `LibraryAnswer` | validation, resource-limit, catalog | C |
| `library.journal` | `after?`, `limit?` (1–500) | `LibraryJournal {changes: [{sequence, actor, request_id, method, label, time_ms, item_count, undoes?, redoes?, undone_by?}], next_after?}`, oldest first | validation, catalog | C |
| `library.inspect` | `sequence` | `LibraryChangeDetail {change, rows: [{item: {kind, …}, before, after}]}` | validation, catalog | C |
| `library.undo`, `library.redo` | `mutation` | `LibraryAnswer`; conflict, naming the items, when a later change touched them | validation, conflict, resource-limit, catalog | C |
| `asset.remove`, `asset.restore` | `targets`, `mutation` | `LibraryAnswer` | validation, resource-limit, catalog | C |
| `catalog.empty-removed` | `mutation` | `EmptyRemovedAnswer {outcome, deleted, deduplicated}`; permission authority only | forbidden, catalog | C |
| `source.check` | `targets` | job `source-check` → `AvailabilityReport {rows: [{asset_id, availability, checked_ms}]}` | validation, resource-limit, cancelled | C |
| `source.missing` | `grouping?` (source-folder) | `MissingOriginals {groups: [{source_folder, volume_id, count, catalog_folders: [{id, name}], reason: {kind: volume-offline, label}, {kind: folder-gone}, {kind: files-gone} or {kind: changed}}], count}` | validation, catalog | C |
| `source.find` | `search_root`, `targets?` or `source_folder?` | job `source-find` → `FindReport {rows: [{asset_id, file_name, result: found {path}, different-bytes {path}, several-identical {paths}, not-found, claimed {path, by} or checking}]}`; changes nothing | validation, read-error, source-unavailable, resource-limit, cancelled | C |
| `source.locate` | `asset_id`, `path`, `mutation` | job `source-locate` → `LibraryAnswer` | validation, read-error, source-unavailable, conflict, cancelled, catalog | C |
| `source.relink` | `pairs: [{asset_id, path}]`, `mutation` | `LibraryAnswer`: every verified pair in one transaction | validation, source-unavailable, conflict, resource-limit, catalog | C |
| `batch.apply-preset` | `targets`, `preset_id`, `mutation` | job `batch-preset` → `BatchReport {done, written, skipped: [{asset_id, code, reason}]}` | validation, resource-limit, cancelled | C |
| `batch.export` | `targets`, `destination` (a folder), `mutation`, `keep_metadata?` | job `batch-export` → `BatchReport` | validation, read-error, resource-limit, cancelled | C |
| `preview.read` | `item`: `{kind: file, file_id}` or `{kind: photo, asset_id, entry_id?}`, `tier` (grid, loupe, large), `priority?` (look-ahead, visible, background) | `PreviewAnswer`: `{state: ready, preview: {item, tier, path, width, height, origin, bytes, key}}` or `{state: queued, job_id, fallback?}`, job `preview-extract` (or `preview-render` for a photograph) | validation, source-unavailable, unsupported-input, resource-limit | B |
| `preview.region` | `item`, `rect: {x, y, width, height}` | job `preview-region` → `RegionAnswer {item, rect, path, width, height, origin}` | validation, source-unavailable, unsupported-input, resource-limit, cancelled | B |
| `catalog.info` | — | `CatalogInfo {path, catalog_id, format, index_format, counts: {photographs, recently_developed, removed, unavailable, folders, collections, picks, indexed_folders, library_changes}, index, previews?}` (`previews` once the preview lane reports its size) | catalog | C |

A preview's `origin` is `exif-thumbnail`, `embedded`, `developed` (a neutral Luxforge development) or `rendered` (a photograph's entry), which every surface that shows it names. `catalog.list` is replaced by `browse.view` and `browse.rows`, and `catalog.import` by picks and `pick.develop`; opening a developed photograph in Develop uses the existing `source.prepare` with an adopt that accepts any preparation the client asked for. A library change records one event naming its sequence (the event's `library_sequence`), and an index or develop batch one event naming the source or the assets, so no batch can overrun the 256-event log.

## Architecture

- **Index lane.** Directory listing and header reads, never image data, two workers on different files, writing the index's own database. The owner is not involved: the index is a cache, not catalog data. One watcher per indexed folder, on the platform's notification API through a pinned dependency or thin per-platform code, feeds the lane changed paths; mount notifications trigger card reconciliation. Every lane publishes to the activity board with progress.
- **Organization.** Events and moments are pure functions of ordered header metadata, the gazetteer and the thresholds; the preview comparison for brackets reads grid previews the preview lane has already decoded. Each is testable in isolation against labelled data.
- **Preview lane.** A priority queue on the owner — the loupe's look-ahead, then visible cells (newest first: the last scroll wins), then the rest of the view (oldest first) — of tasks, each one file's tier, deduplicated so a second request joins the task and raises its priority, and bounded at 20,000 (the design's 10,000 files in view, two tiers each; `resource-limit` past it). At most two worker threads, started on the first task and each blocked on its own channel while idle, extract embedded previews through `luxforge-raw` and make scaled JPEG decodes through `luxforge-jpeg`; the owner hands the highest-priority task to an idle worker, and the worker reads the file, decodes, resamples, encodes, writes the file and its row through its own index connection, evicts, and posts its outcome back. The owner does only SQL and bookkeeping. Before reading a file a worker compares its signature with the index row's, and refuses a changed file (`source-unavailable`, nothing written); it checks again before writing and discards what it made if the file changed meanwhile. A client's `preview.read` of a missing tier opens one catalog job per task (`preview-extract`, shared by every request for it, its result the preview); `job.cancel` removes the task from the queue or stops it at its next checkpoint, unless a view still wants it. `want_view` (for `browse.view`) queues, in the background, the grid tiers a client's view lacks as one job per client, "Reading previews", with an `n of N` count and a truthful fraction on the activity board and its `job_id`, replacing the client's previous one; its cancel drops the tasks only it wanted, and its running tasks finish into the cache. `OwnerHandle::watch_previews` wakes a client on the owner thread when a preview its request or view waits on is written; nothing polls. The lane never touches the editor's one-slot source cache.
- **Develop lane.** One file at a time per worker, off the source worker, so developing picks never waits behind or delays a Develop preparation; the RAW interpretation is read without demosaicing, and whether any format needs its mosaic unpacked to fill it is measured.
- **Views on the owner** (built: `crate::browse`, `api/owner/views.rs`). One ordered item list per client, 16 bytes an item, evaluated over the index (files) or the catalog (photographs), so a window, a range selection, the loupe's next frame and the filmstrip's neighbours cost no query. The owner does SQL and bookkeeping only, and reads no file:
  - **Evaluation.** `browse.view` reads the source's compact columns in one pass — an event's files through the event cache, a folder's or a card's by range over the index's folder key, photographs from the assets and capture rows — with each folder, body, lens and place stored once. A file's pick and its developed photograph (a photograph, not removed, whose original's canonical path is the file's) are read per folder of the view by range over the catalog's keys, and its availability from the index's offline roots. A source of more than `MAX_VIEW_ITEMS` items is refused with `resource-limit`, whatever the filter.
  - **Moments without a pick** is decided per frame over the whole source, before any other condition: the source is grouped Day › Camera › Moment under the query's thresholds, and a frame is left out when it is picked or its moment has a picked frame. It is the moment as shot, whatever the other conditions, the sort or the grouping. Every other condition is a predicate of one item, so a facet count is exactly the size of the view it predicts.
  - **Order.** The capture-time sort orders with the organize functions under the query's grouping, which applies only to it; the other sorts order by their key, reversed when descending, then capture time (undated last), the file name ignoring case, and the path (folder, then name) for files or the row for photographs. A smart collection evaluates its stored source and filter, then the view's own; one that names a smart collection, or whose query is over files, is refused.
  - **Staleness.** A view is stamped with the catalog's latest library change and the index's revision, read before its rows. It is marked stale after every change the owner records and whenever its session is read (`session.state` and every answer that carries the session); `browse.rows` and `browse.select` check it too. A stale view still answers rows for its items, refuses a window whose item has gone (`conflict`), and is refused as a library change's `{kind: selection}` target; the client evaluates it again.
  - **Windows and selection.** `browse.rows` reads a window in a fixed number of statements whatever its size. `browse.select` changes the selection's position ranges without reading the view, except that naming items finds them in one pass; a view evaluated again carries the selection and the active item over by item.
  - **Events** are computed on demand from the files under the indexed folders (online or offline) and the mounted cards, and cached by index revision and event thresholds, two computations at most, each holding its events' summaries and file rows; `event.list` counts picks afresh on each call, and an event source resolves its identity through the cache under the view's thresholds.
  - Until the gazetteer lands no file has a place; until the preview lane lands a row's preview state is read from the index's preview records and nothing asks for previews.
  - If evaluation at the design scale misses its budget, catalog views move to a read-only connection, which needs the catalog's exclusive locking revisited ([P13](#proposals)).
- **Desktop.** The Select workspace is its own seam within the enforced layering: `app/select.rs` (its messages in `app/message/select.rs`), `state/select.rs` and `view/select.rs`. The view model holds what Select last read — the event list, the view's summary and facets, and a window of its rows — and derives the regions from it after every message; with Develop shown it derives nothing. Every change of source, filter, sort or grouping sends `browse.view` with the whole query, followed in the same owner task by `session.state` for the selection the owner carried over; `browse.facets`, `event.list` (one in flight, the newest search text waiting) and `browse.rows` are owner tasks whose answers carry the evaluation or view revision they belong to, so an overtaken answer is dropped. The summary's group layout becomes the grid's blocks once per summary or collapsed burst (days, camera groups, moments with their kind, span, steps and evidence, the singles between them), and the `luxforge-ui` layout is rebuilt only then, relaid out for a new width or cell size and never built in `view()`. Rows are read in aligned blocks of 200 for the cells on screen and one screen either side, one request at a time, at most 16 blocks kept and the furthest dropped first, so a 10,000-file view holds only what is near the screen; a block the owner refuses is not asked for again until the view is evaluated again. `browse.select` is session-only and runs synchronously in the update of the click or key ([performance rule 12](../engineering/performance-rules.md#rules)); the grid draws the selection and active item the session reports for the revision on screen. The owner's wake for another client's change, which the event sync already receives, reads `session.state` while Select is shown (and once on showing it after a wake while hidden); a view the session reports stale is evaluated again with its query, the scroll kept near the active item, and the events read again. A thumbnail's image handle will be made once and kept while its cell is on screen (a handle created in `view()` uploads again every frame, as the Develop surface learned); the loupe will be its own surface with the look-ahead cache. Develop keeps one document; switching photographs replaces it through the open path with the large preview drawn first, and neighbouring photographs are not prepared ahead in the first version.
- **Select widgets.** `luxforge-ui` holds the Select workspace's widgets, used by the Select shell except the loupe's, the filmstrip, long-running work and the catalog search field, each plain data and callbacks, drawn to the boards and shown on the components gallery's five Select pages:
  - `thumbnail_grid` over a `GridLayout`: the view's days, cameras, moments and singles as blocks that flow into lines as the boards' CSS does, a moment framed with its header (kind, span, picks, evidence tag, action), a collapsed burst one cell with its count, a moment wider than the view wrapping inside one frame. The layout is computed once per grouping, width or cell size, with prefix-sum lines, so the visible range, a hit test, an item's cell, `reveal` and arrow-key `neighbour` are binary searches; the widget draws only the visible lines with renderer primitives and asks the caller for the visible cells alone, as plain data borrowing the caller's image handles. Scroll is held by the caller; the widget publishes presses (with modifiers and double click), scrolls, a moment's action and its own size. Select (136 × 122 pt) and catalog (168 × 176 pt) cell presets scale with a size slider. The loading placeholder takes the photograph's shape from its header; offline and unreadable files carry a badge in the clipping red.
  - The sources panel's headings, month labels and rows (volume dot, picks over total, the red count of Missing originals, dimmed offline rows) and search field; the filter bar's segments with counts, chips with menus or a clear cross, and the catalog's search field; the title bar's workspace switch and Develop N (ready, busy with its bar, disabled at 0); long-running work as the status bar's busiest job, a Performance row with an estimate and Cancel, and the in-view progress sheet, each drawing exactly the fraction it is given or none; the floating strip (`select_strip`: Grid, Loupe, the sort with its menu and the size slider); the loupe's info bar, its numbered frame strip over a caller's window of frames, the 100% inset labelled a camera preview or a Luxforge development, the region box and the key hints; and the Develop filmstrip over a caller's window of the set.

## Delivery plan

The work divides into four lanes that run in parallel after one contracts task, so at most four implementers work at once, each in its own worktree, integrated on one branch. The task list is [tasks/catalog.json](../../tasks/catalog.json); its first task is the owner's answers to the [proposals](#proposals).

### Contracts first

One task fixes everything the lanes share before any lane starts, so no lane waits on another's internals:

- **Storage.** Catalog format 12 (picks, indexed folders, catalog folders, capture, volumes, collections, the journal, the new asset columns) and the index database's schema, created and refused as the design says.
- **Types.** The shared shapes in one core module: file and photograph identities, header metadata, events and moments, view queries and rows, picks, library changes, job progress. Every lane codes against these.
- **Method declarations.** The design's API written down as parameter and result shapes with error codes, in the design's [API](#api) table and the types module. Each method is registered in the method table by the lane that implements it, when it works, so `schema.list` never lists a method that does nothing.
- **Generated data.** `cargo xtask generate-catalog` writes deterministic indexes and catalogs at the design scale — trips with positions, bursts, brackets with and without exposure bias, undated files, picks, catalog folders, collections, missing originals — with no image files, plus small generated image folders with real embedded thumbnails for the preview and desktop lanes. Every lane tests against these, so the desktop is built before the core lanes finish.
- **Module skeletons.** Empty modules in the places each lane owns (below). The owner gets one file per lane for that lane's owner-side state, worker messages and handlers (`api/owner/files.rs`, `previews.rs`, `library.rs`, `views.rs`). The hot shared files get a marked section per lane, so parallel work rarely touches the same lines: `api/methods.rs`, `jobs.rs`, `editor/catalog.rs`, `editor/catalog_rows.rs`, `lib.rs`, and the desktop's `app/keymap.rs`, `app/message.rs`, `app/mod.rs` and `state/mod.rs`.

### Lanes

| Lane | Delivers | Owns | Consumes |
| --- | --- | --- | --- |
| **A · Files** | Header metadata, the index lane with reconciliation and exclusions, watchers and cards, events and moments from metadata | `core/src/index/`, `core/src/organize/`, `export/metadata` (typed accessors), the gazetteer asset | Contracts |
| **B · Previews** | The preview lane and cache, embedded extraction per camera, the preview-brightness bracket check, the 100% region and development fallback, rendered previews of developed photographs | `core/src/previews/`, `luxforge-raw` (embedded previews), `luxforge-jpeg` (scaled and cropped decode) | Contracts; the index's file identities (A) |
| **C · Catalog** | Picks and the library journal, catalog folders and collections, developing picks, removal, batch jobs, availability, Locate and resolving missing originals | `core/src/library/`, the develop lane, `editor/source.rs` changes | Contracts; header metadata (A) for developing |
| **D · Views and desktop** | Browse views, facets and selection; the Select workspace, loupe and compare, the Develop confirmation and filmstrip, catalog browsing, missing-originals UI, long-running-work UI | `core/src/browse/`, `app/select*`, `state/select*`, `view/select*`, new `luxforge-ui` widgets, the new smoke scenarios | Contracts and generated data first; each core lane's methods as they land |

Each lane works through its tasks in order: A reads metadata, then builds the index lane, events and moments, then the watchers; B builds the preview lane, the 100% region and rendered previews, then the bracket check once events and moments exist; C does picks and the journal, folders and collections, developing picks, availability and Locate, resolving missing originals, then removal and batch jobs; D builds browse views once events and moments exist, then the Select workspace, long-running work, the loupe, the Develop confirmation and filmstrip, catalog browsing and the missing-originals view. The plan lists the order with its task IDs.

A lane's task says what it consumes and produces. A lane that needs another's result before it has landed works against the generated data and the declared shapes, and replaces its fake with the real call when the other task is merged.

### Integration

- An integration branch holds the contracts and every merged task; each implementer starts from it and rebases on it.
- While building, each task runs only its own tests (`cargo test -p CRATE FILTER`); `verify --tier quick` once at hand-off. The integrator reviews every diff and runs `rendered` when a desktop task merges.
- Timing runs, the design's performance targets included, happen once, in the last task, on the integrated build.
- Every task answers the performance-rules checklist for what it changed; any new dependency (a watcher crate, the gazetteer data) is pinned and noted in the dependency policy, with the manual review still deferred.

## Performance

Provisional targets, measured once on the owner's M4 when the feature is complete, with a real 1,000-frame RAW trip on the internal SSD and on an SD card reader, a generated 10,000-file folder and a generated 100,000-photograph catalog, and recorded with their scope in [performance](../specs/performance.md). Nothing is claimed until measured; misses are reported with figures.

| What | Provisional target |
| --- | --- |
| Browsing a 1,000-frame card or folder for the first time, internal SSD | The first screen of the grid within 1 s; every file, event and moment within 5 s; grid previews for all reported. From a card reader: reported |
| Returning to a known 1,000-file card or folder | Reconciled and drawn within 1 s |
| First index of a 200,000-file folder | Reported, in the background, with the editor responsive throughout |
| Grid scroll over 10,000 files | Presented frames p95 within 16 ms at 120 Hz |
| Loupe stepping with the look-ahead warm | The next frame presented in the frame after the key; a held arrow presents every frame at the key-repeat rate |
| 100% focus check | From a full-size embedded preview, within 50 ms of `Z`; from an on-demand development, reported per camera |
| Bracket detection from previews | Under 1 ms a run, on decoded grid previews |
| Developing 20 RAW picks | Reported; Develop shows the first photograph's preview as soon as it is committed |
| Switching photographs in Develop with a cached large preview | Presented in the frame after the key |
| `browse.view` over 10,000 files or 100,000 photographs | p95 under 50 ms; `browse.rows` of 200 under 2 ms |
| Memory | Decoded grid and loupe previews in the desktop under byte budgets (provisionally 192 MiB and 256 MiB) whatever the view's size; the owner grows by the view's id list |

The performance-rules checklist is answered in the implementation tasks: browsing decodes only embedded previews, never an original's image data; the only new development is the 100% fallback and the rendered catalog previews, each bounded to one RAW at a time off the editor's cache; the owner's new work is SQL and bookkeeping; the only new timers are the availability and card checks, gated on an open catalog.

## Scope

**In:** cards, folders and indexed folders; the index, with change notifications and reconciliation; progress for every long-running job; events by time and place with offline place names; days, cameras and moments (bursts, brackets from metadata or previews); the Select workspace with grid, loupe, 100% focus check and compare; picks as journaled library changes; developing picks and sending unedited ones back; Develop's workspace switch and development-set filmstrip; the catalog's search, metadata browser, collections and smart collections; availability, Locate and resolving missing originals per photograph by source folder; remove, restore and empty; batch preset and export; the index and previews cache; the whole API.

**Later, each by its own decision:** merging brackets to HDR and stitching panoramas (a merge source kind, [above](#hdr-and-other-merges)); panorama and visual-similarity grouping; sharpness or closed-eye hints to help choose within a burst; renaming or hand-correcting events and moments; copying files from a card on develop; ratings, keywords, flags or colour labels; custom collection order; virtual copies (versions cover a second interpretation); sidecars and writing metadata to files; catalog backup, portability and sync; neighbouring-photograph prefetch in Develop.

**Not in scope:** anything that writes, moves or deletes a file; online place lookups; cloud or multi-user catalogs.

## Acceptance

- Browsing, indexing, picking and developing never change a file's bytes, time or location (checked by hash and `stat` before and after), and browsing reads no image data beyond embedded previews and the 100% fallback.
- Events and moments agree with an independent grouping of the same metadata and previews over a labelled corpus of trips, bursts, metadata brackets, metadata-less brackets and single frames from several camera makes, with the thresholds and any misgrouping recorded.
- Developing proposes the event's catalog folder, or the existing folder for a later pick from the same event; catalog folder renames, moves, merges and deletions are journaled and never touch a file on disk.
- Picks survive a restart; pick and clear are journaled with their actor; undo and redo restore exact prior values and refuse, naming the items, when a later change touched them.
- Developing creates exactly the picked photographs, with fingerprints, interpretations and Original entries; cancellation keeps what was committed and nothing partial; an identical file links rather than duplicates; a missing original relinks only on a fingerprint match; a card's pick uses its copied file when the fingerprint matches.
- Every Select gesture's request is the request its API equivalent sends, and an agent's picks appear in the desktop without a reload.
- The loupe never shows a frame under another frame's name; previews report what they are; deleting the index directory loses nothing but time.
- Offline files and offline photographs stay browsable; Develop and export refuse with Locate offered; resolving missing originals relinks only fingerprint-verified files, reports every other photograph with its reason, and changes nothing on stop or failure.
- Empty Removed is refused to a live-session client.
- The measurements above are recorded, and the rendered `select`, `loupe`, `develop-picks`, `filmstrip` and `resolve-missing` smoke scenarios pass with correlated state and logs.

## Proposals

Recorded defaults awaiting the owner's decision; the plan's first task asks for them.

| # | Question | Recommended default | Why |
| --- | --- | --- | --- |
| P1 | Browse, pick, develop | Files are browsed through the filesystem and an automatic event layer, never imported; only developed picks enter the catalog; nothing about unpicked files is kept but a disposable index | The owner's direction; the catalog stays the size of the work, and selecting is fast because nothing is hashed, developed or managed |
| P2 | Ratings, keywords and flags | None, in selecting or in the catalog; collections, places and dates organize the catalog | Selecting is the one decision; a second rating system would repeat it (owner, 2026-09-30, pending confirmation) |
| P3 | Event rules | A new event at a gap over 3 hours, unless it is under 24 hours with positions within 25 km on both sides (a stay), or at a jump over 25 km from the event's last position | Keeps a trip together across its nights and separates outings and trips, with or without GPS; checked on generated data, a labelled corpus of real trips outstanding |
| P4 | Place names | An offline gazetteer of populated places bundled with Luxforge (for example GeoNames `cities15000`, CC BY 4.0, about 30,000 places, a few megabytes compressed), subject to the deferred asset review; no online lookups | Names events and makes places searchable without sending positions anywhere |
| P5 | Moment rules | Runs of one body under 1 s apart, or under 2 s with matching aperture, ISO and focal length (up to and including them for clocks of whole seconds); a run is a bracket when exposure steps by ⅓ EV or more in the metadata (nominal values within 0.13 EV), or, lacking metadata, by about ⅔ EV in the previews with unchanged framing; 2–9 frames, and a longer run repeating one bracket is several. Adjustable per view | Burst by speed; bracket by exposure, from metadata first and previews otherwise, as the owner asked; checked on generated data, to be checked against a labelled corpus before it ships |
| P6 | What the loupe and 100% check show | The camera's embedded preview, labelled; where it is smaller than the sensor, a neutral Luxforge development of that frame for the 100% inset, on demand | Instant for most cameras; honest where the preview cannot show focus |
| P7 | Picking a burst frame in the loupe | Moves on to the next moment | One frame of a burst is usually the one |
| P8 | Cards and cache budgets | Browse cards in place; before developing from removable media, use a copied file when its fingerprint matches, otherwise confirm; loupe and large previews share 4 GiB with LRU eviction | Luxforge references files where they are; copying is the open storage question |
| P9 | Sending a developed photograph back | Allowed while it has only its Original entry; after an edit, only Remove | Changing your mind before editing costs nothing; nothing with history is erased |
| P10 | Undo across workspaces | Library undo in Select, history in Develop | Keeps both undo models exact |
| P11 | Resolving missing originals and batch export in the first version | Yes: per-photograph resolution grouped by the folder on disk each was developed from, searched by name and size and verified by fingerprint; batch export with the existing JPEG settings only | Catalog folders need not mirror the disk, so recovery follows the recorded source folder; it is the owner's moved-drive workflow |
| P12 | Who may empty Removed | The desktop and `--permission-authority` only | An agent should never be able to erase history |
| P13 | Scale and where views are evaluated | 10,000 files in view and a 100,000-photograph catalog; views on the owner; a read-only catalog connection only if a measurement fails | One writer and one connection until a measurement says otherwise |
| P14 | How files come in | Cards, folders and Add a folder… replace import; a single opened file is picked and developed at once | One way into the catalog; today's single-photograph flow keeps working |
| P15 | How the catalog is organized | Catalog folders, one per photograph: Develop proposes a folder named after the event (Return accepts, typing renames, or choose an existing one), later picks from the same event default to it, and folders are then freely renamed, nested, merged and reorganized; collections for sets across folders | Gives the catalog the event organization by default, a name chosen at the natural moment, and full control afterwards, without recomputing anything the person may have changed |
| P16 | Naming | The filesystem section is "On disk"; "folder" in the catalog always means a catalog folder | Two kinds of folder in one panel would otherwise be ambiguous |
| P17 | Keeping the index current | Platform change notifications for indexed folders (FSEvents with replay on macOS), re-listing cards on mount and network volumes on open, reconciling by signature, carrying picks across moves by file identity, and skipping packages, caches and hidden and system folders | No polling, fast returns, and a whole `~/Pictures` indexable without surprises |
| P18 | Showing long-running work | Every job over a second in the Performance section with progress, estimate and Cancel, and the busiest in the status bar; a progress sheet only in a view with nothing to show yet, never over the whole window | The owner's requirement that intense work is always visible, without turning the app modal |
