# Development, verification and packaging

Build, editor and verification tooling uses Rust: `cargo xtask <command>`. Commands reject unknown arguments and pass paths to child processes without shell interpolation. `cargo xtask help` lists those commands. The optional [extended camera corpus](#extended-camera-corpus) uses Python and AWS CLI for cloud transfers.

## Setup

Install Git and Rust through rustup plus the platform prerequisites in [platforms](platforms.md). Nothing here installs system tools silently.

```sh
rustup toolchain install 1.94.0 --profile minimal --component rustfmt --component clippy
cargo xtask doctor
cargo xtask check
cargo xtask build --release
```

Doctor reports missing tools and the graphics environment without installing anything; it does not prove a desktop or GPU is available. The first build fetches pinned crates and needs network.

## Commands

| Purpose | Command |
| --- | --- |
| Environment report, with the graphics adapters a built release editor sees | `cargo xtask doctor` |
| Full local and CI checks: repository links and task plans, formatting, Clippy, tests | `cargo xtask check` |
| The same without the slow tests and the doctests, what the quick tier runs | `cargo xtask check --quick` |
| The quick tier with its summary, with no release build | `cargo xtask verify --tier quick --output NEW_DIR` |
| A whole verification tier with one summary | `cargo run --release --locked --package xtask -- verify --tier quick\|rendered\|timing\|full --output NEW_DIR [--jobs N] [--binary PATH] [--manifest FILE]` |
| Individual steps | `cargo xtask check-repository`, `fmt`, `lint`, `test [--quick]`, `build [--release]` (the editor and the headless `luxforge-json`) |
| Run the editor, release build | `cargo xtask develop [--catalog FILE] [--open PATH] [--data-root DIR]` |
| Run a lightly optimized debug build, debugging only | `cargo xtask develop --debug ...` |
| Run an agent's editor check without taking focus (macOS) | `cargo xtask develop --background --catalog FILE [--open PATH]` |
| Display-independent acceptance of what `cargo test` cannot prove at the same layer: the Basic and histogram, field-patch conformance (in release), Presence, mixer and vignette, Tone curve and masking chapters | `cargo run --release --locked --package xtask -- editor-acceptance --output NEW_DIR` |
| Core timing on a real-sized JPEG; `--lens-only` accepts JPEG or RAW and measures profile queries, commits, matched exact renders, point picks, serial export and cancellation | `cargo run --release --locked --package xtask -- editor-performance --source JPEG --output NEW_DIR [--samples N]`; for Lens, `--lens-only --source JPEG\|RAW` |
| Headless Detail full render/export, reopened-owner neutral picks, first/later colour-limited draft ticks and neutral source sharing; accepts the planned 24 MP or 60 MP JPEG | `cargo run --release --locked --package xtask -- detail-performance --source JPEG --output NEW_DIR [--samples N] [--case all\|render\|export\|points\|sharing]` |
| Detail value-mask input grids through the production coverage evaluator: dense overlay 2880×1800 and sparse thumbnail 28×19, first build and mask-amount-only reuse | `cargo run --release --locked --package xtask -- detail-grid-performance --source JPEG --output NEW_DIR [--samples N]` |
| Desktop slider/curve-to-presented-frame and settled-histogram timing, peak RSS, scratch and idle CPU; `--zoom` selects a percentage view, `--moving-pan` interleaves pan with a paced burst, and `--mode crop-start` times opening a crop draft and reads its memory. `--curve-layer` commits a nonneutral global Tone curve before a numeric-source workload, independently of the `--control curve` gesture target. `--detail` commits moderate sharpening 60, luminance 40 and colour 40 before the gesture. `--presence` commits a Presence layer with all three fields at +100 before a drag, commit or crop-start. `--lens` selects the first eligible offline profile through the desktop control, with explicit acknowledgement for a JPEG; `--perspective` seeds +20 horizontal and -10 vertical. `--action`/`--parameter` measure another drafting slider in place of Basic exposure: a field-patch slider (presence, mixer, vignette, ...), or the RAW white balance `set-raw` `temperature` or `tint` over a RAW `--source`. `--control curve` drags the developer proof curve; with `--action set-curve --parameter luminance` (any curve parameter of a non-developer field-patch action) it seeds the Tone curve with `[[0, 0], [0.5, 0.5], [1, 1]]` and drags that mid-tone point instead, in drag or commit mode. Every frame is timed on the path that drew it, GPU or CPU, and reported with its path and reason; `--warm MS` leaves the editor alone before a drag so the committed stack's GPU programs compile, `--contend N` queues N exports, which the GPU tile worker streams beside a drag, and `--idle` also checks idle after the release's dissolve. `--reference-renderer` launches the editor with `--no-gpu-render`, refusing its GPU stage, so the reference renderer draws every frame of a drag, commit or stroke: the same build's baseline for a GPU run. In paint mode `--masks N` (1 to 16, the masks a recipe holds, the brushed one among them) adds radial masks across the frame after the brushed one, each holding the masked exposure, made and adjusted through the actions with the mask named so 16 masks fit the script's 64 steps, and `--mask-presence` gives every mask a masked Presence layer of Clarity 50 and Texture 40 — the cap of 16 masked spatial layers, read from the core — and `--presence` commits a global Presence layer of Texture 25 and Clarity 20 under them; the run's deadline allows 5 s for each mask after the first. `--window WIDTHxHEIGHT` opens the editor's window at that many logical points, such as `1728x1080` for a full-screen Fit stage on the M4's 3456 × 2160 display. | `cargo run --release --locked --package xtask -- editor-latency --source JPEG\|RAW --output NEW_DIR [--binary PATH] [--samples N] [--mode drag\|commit\|burst\|paint\|hover\|crop-start] [--zoom PERCENT] [--moving-pan] [--control slider\|curve] [--action ID --parameter NAME] [--crop DEGREES] [--basic] [--presence] [--curve-layer] [--detail] [--lens] [--perspective] [--mask] [--masks N] [--mask-presence] [--mask-overlay] [--reference-renderer] [--window WIDTHxHEIGHT] [--idle] [--warm MS] [--contend N]` |
| Verify golden fixtures; generate 24 MP, 60 MP, the mixer and presence scenarios' own hue-wheel and gradient/edge/texture/flat workloads, the `mask-range` scenario's own colour-chart patches and the `curve` scenario's grey ramp and colour patches and the `compare-zone-plate` scenario's 6000 × 4000 zone plate | `cargo xtask fixtures`, `cargo xtask generate-fixtures [--output NEW_DIR]` |
| Catalog test data, deterministic from the seed (default 1), from one shoot plan (trips across six bodies, with and without GPS, a same-day jump of over 25 km, bursts, brackets with and without exposure bias and with no metadata step, undated files; bodies that write no time offset keep UTC). `--images N` (93 to 5,000): N real 640 × 427 JPEGs with full EXIF and a 160 × 107 embedded thumbnail, laid out as a camera card (`images/card/DCIM/100NZ8_1/`, `100NZ8_2/`), card dumps (`images/Card dumps/<date>/`), user-named folders (`images/2026-09-14 Lake/`, `images/From Anna/`) and `images/iPhone export/`, with their ground truth in `images/manifest.json`. `--files N` (at least 93): a format-13 `catalog.sqlite` and its index `catalog.index/index.sqlite` of N files with no image files, on a fictional disk (a mounted `NIKON Z 8` card, `/Users/generated/Pictures` and the unconnected `Photos SSD`), some picked and some developed already. `--assets M`: M developed photographs in `catalog.sqlite` (those developed from the files, then older trips over several years), with catalog folders, collections, a smart collection, and removed, offline, missing and changed originals; a million takes under a minute in a release build | `cargo xtask generate-catalog --output NEW_DIR [--files N] [--assets M] [--images N] [--seed N]` |
| Adding a camera: download selected CC0 samples from the raw.pixls.us index, verify their SHA-256 and read each with the RAW adapter, or see why it refuses them | `cargo xtask raw-camera-metadata --index FILE --ids ID[,ID...] --output NEW_DIR [--max-source-mib N]` |
| Regenerating the bundled gazetteer from a downloaded GeoNames `cities15000.txt`, offline ([dependencies](dependencies.md#bundled-place-names)) | `cargo xtask gazetteer --source FILE --output NEW_FILE` |
| Adding a camera: a DNG or TIFF's IFDs, geometry and calibration tags and opcode-list layouts, read-only | `cargo xtask inspect-dng --source DNG [--json NEW_FILE]` |
| Authentic RAW editor journey and reopen over one file the RAW manifest lists | `cargo xtask smoke --scenario raw-editor --source RAW --manifest FILE --output NEW_DIR [--binary PATH]` |
| Rebuild the pinned offline Lensfun resource | `cargo xtask lensfun-import --source UPSTREAM_DIR --output NEW_DIR` |
| Lens selection/export and marked-edge qualification through the JSON API | `cargo xtask lens-qualification --manifest FILE --edges FILE --output NEW_DIR` |
| Rendered Lens correction and Perspective workflow | `cargo xtask smoke --scenario lens-perspective --output NEW_DIR` |
| Native macOS visibility gate and three 30-second external CPU windows each with Performance expanded, collapsed and window hidden, over the generated 24 MP photo; transparent background-only window, no activation | `cargo run --release --locked --package xtask -- smoke --scenario visibility-monitoring --output NEW_DIR` |
| Rendered smoke scenario, needs a native graphical session | `cargo xtask smoke --scenario NAME --output NEW_DIR [--binary PATH]` |
| Every smoke scenario, with its launches and frame counts, what it opens and its window | `cargo xtask smoke --list` |
| A recorded smoke run's checks again, over a copy and without launching | `cargo xtask smoke --verify-only RUN_DIR --output NEW_DIR [--scenario NAME] [--source RAW]` |
| Rendered crop workflow and overlay | `cargo xtask smoke --scenario crop --output NEW_DIR`, `--scenario crop-draft` |
| Rendered image information: key and palette toggles, capture fields, selected/history/live crop dimensions, zoom and hidden panels, without photo uploads | `cargo xtask smoke --scenario information --output NEW_DIR` |
| Rendered workspace panels, mode, preview, the combined crop section’s transform icon row, an agent's conflicting commit and palette; unavailable-provider notice | `cargo xtask smoke --scenario workspace --output NEW_DIR`, `--scenario unavailable` |
| Rendered Basic slider gesture: draft, commit, typed value, undo, reset and an agent's conflicting commit | `cargo xtask smoke --scenario basic --output NEW_DIR` |
| Rendered Basic panel on a JPEG: all three groups, the White balance group's four controls (Temperature, Tint, Neutral picker, As shot) with As shot sending Basic's own 0 and 0, historical values, a group reset, the neutral picker, As shot after a warm drag, and the default screen with Basic expanded and every other section collapsed | `cargo xtask smoke --scenario basic-panel --output NEW_DIR` |
| Rendered histogram, clipping overlays, a `render.sample` of the edited pixel and a drafted frame, each report held to an independent reduction — the reference's exactly, the GPU's (the Original undone to and an exposure released on the GPU) its clipping counters within 0.1% of the output pixels and `analysis.request`'s bins within a quarter of a code by the earth mover's distance — a GPU drag's counts marked updating, and `analysis.request` a ready hit on the GPU's report | `cargo xtask smoke --scenario histogram --output NEW_DIR` |
| Rendered Basic composed with crop and straighten | `cargo xtask smoke --scenario basic-crop --output NEW_DIR` |
| Rendered restart: a Basic edit committed in one launch and reopened in the next | `cargo xtask smoke --scenario basic-restart --output NEW_DIR` |
| Rendered Presence: section expand, a Clarity drag and cancel, Texture and Clarity each committed at Fit and 100%, Dehaze at both signs, all three fields at once through the raw API and the module reset, over a generated gradient/edge/texture/flat fixture | `cargo xtask smoke --scenario presence --output NEW_DIR` |
| Rendered Colour mixer: section expand, a Red hue drag and commit at Fit and 100%, a Saturation group reset, a stronger hue shift and the Saturation and Luminance tabs, over a generated hue wheel | `cargo xtask smoke --scenario mixer --output NEW_DIR` |
| Rendered Tone curve over a generated grey ramp and the orientation fixture's four colours: section expand, three on-diagonal points through the API (identity bytes), a point drag held and released, a point added and one removed, the Points list opened, a fourth point added and an S-curve typed one coordinate per Enter, the module reset from the band, and a radial mask with the curve dragged through it (scope chip); against the neutral frame, no new decrease along the ramp rows, channel spread within one code, each unclipped patch's Oklab hue kept within 1°, the S-curve lowering the ramp's lower half and raising its upper half, and an identity curve drawing the neutral pixels exactly | `cargo xtask smoke --scenario curve --output NEW_DIR` |
| Rendered Vignette: section expand, an Amount drag and commit at Fit and 100%, roundness and feather extremes, a post-crop recentre and the module reset | `cargo xtask smoke --scenario vignette --output NEW_DIR` |
| Rendered Before/After at Fit over the generated 24 MP zone plate: After's retained exact raster drawn below its size, judged by the zone-plate statistics (the standard deviation and mean of its luma beyond the display's Nyquist limit against an independent linear-light box reduction, so full-contrast replica rings fail it) with its mip levels in the resident photo-slot bytes and none for the display-size photograph; the display-size photograph drawn at Fit before the comparison is the control | `cargo xtask smoke --scenario compare-zone-plate --output NEW_DIR` |
| Rendered Before/After at Fit over the generated 60 MP JPEG, whose exact raster the device's textures hold in tiles and cannot give mip levels: After alone must equal the display-size photograph drawn before the comparison, with no mip level resident | `cargo xtask smoke --scenario compare-tiled --output NEW_DIR` |
| Rendered GPU preview stage at Fit: the orientation fixture drawn through the photo surface's GPU stage with the identity program, launched with the `--evidence-gpu-identity` test hook; `load`'s fixture, placement and readiness checks over the GPU-drawn frame, which must record the `gpu` drawing path, no fallback, the boundary of the frame on screen with the CPU frame not drawn, at least one pass, GPU-preview figures within the 2 GiB budget and the status bar's `GPU preview · N ms` with that frame's own figure | `cargo xtask smoke --scenario gpu-identity --output NEW_DIR` |
| Rendered GPU preview gestures at Fit over the orientation fixture: a Basic drag whose first tick holds the frame on screen while the surface evaluates the one boundary and whose later ticks are drawn on the GPU with no preview job, each GPU frame's boundary, draft revision, budget figures and `GPU preview · N ms` label checked against its state and its pixels against the frame its release commits, then the boundary kept on the GPU as the resident one once settled, within the budget; a Detail drag drawn from that resident boundary from its first tick, asking for none, its pixels against the frame its release commits; in Mask mode under the coverage tint, a linear gradient's resting middle handle pressed and moved twice on the GPU without letting go, with the overlay following it, its pixels against the frame its release commits, and a brush stroke painted a position per tick, its positions on the GPU; for the settle, each commit replacing a GPU frame dissolving from that frame's revision and boundary, the drag's GPU frame against its settled frame within the pointwise limits, an `idle` step after the dissolves drawing nothing of its own (an idle step's settle lasts while the GPU stage still compiles, at most a minute more, since a warm-up's end wakes the editor), a drag with both clipping overlays drawn on the GPU with its own marks, and a release's dissolve cancelled by a clipping toggle and by the next gesture where it still runs when they take effect, one that ended first recorded with both times; then in the pointer mode, over a committed Dehaze and Clarity, a Texture drag, a Clarity drag and a Basic drag under Presence, each a tick a step, each begun once a `gpu_warmed` step has seen the committed stack's warm list compile (at most 60 s after the commit's quiet; a drag that begins with anything still compiling fails) and no frame of it waiting for a compile, every GPU tick drawn with no preview job, its compute passes counted from `gpu_preview_spatial_passes` (at most five for a tick that moves only Texture's or Clarity's gain), its plan's lights named — the Presence drags' the stand-in with the Detail committed earlier left out (`stand-in`), the Basic drag's computed every tick from the source (`source`) — and its pixels against the frame its release commits, and the launch's first warm-up recorded once it ended, with its figures (`gpu_warm_up`, `first`) | `cargo xtask smoke --scenario gpu-preview --output NEW_DIR` |
| Rendered GPU preview drags at percentage zooms over the orientation fixture: the Basic drag at 100%, its boundary held for the visible region from one request and its later ticks on the GPU with no preview job of any kind, and at 200%, where the whole photograph is still in view, drawn from that boundary, kept resident, from its first tick, each GPU frame's boundary, draft revision, budget figures and label checked against its state and its pixels against the frame its release commits, at 100% over a committed Dehaze and Clarity a Texture and a Clarity drag on the GPU, their compute passes counted (at most five a tick), and a Basic drag under that Presence on the GPU too, its light computed every tick, each naming its lights `source`; with Detail committed under Presence and Dehaze, a Texture drag and a Detail drag on the GPU, their light the stand-in with Detail left out (`stand-in`), each of these 100% drags held after its first tick until the sequence its region plan asked for has compiled (a `gpu_warmed` step, at least 4 s and at most 60 s), since a percentage view's plans are not warmed; and at 800% panned past its region as it ticks, the old boundary let go, the new region's asked for and every GPU frame's region holding the view recorded with it; each Basic release's committed frame dissolving in from the drag's GPU frame, its identities checked and the capture recorded as during or after it; then back at 100%, the whole stage in view and a drag there on the GPU; and, in a second short launch, Basic drags at 50% and then 33%, where the view draws the displayed-size reduced stage of the whole photograph: each drag's boundary held for that stage from one request (its bounds the view's), its later ticks on the GPU from a whole frame's plan with no preview job and nothing said in the status bar, each GPU frame's boundary, draft revision, budget figures and label checked against its state, the same settings drawn twice to the same bytes, its pixels against the frame its release commits and within the pointwise limits of the settled frame, the release's committed frame dissolving in from the last GPU frame, and the boundary kept resident | `cargo xtask smoke --scenario gpu-preview-zoom --output NEW_DIR` |
| Rendered percentage zooms: 50%, 100%, 120%, 800% and 1600%, pans to the centre and the far corner at 1600%, and idle checks at Fit, 100% and 1600%, over the generated 24 MP and 60 MP JPEGs, one launch each | `cargo xtask smoke --scenario zoom --output NEW_DIR` |
| Rendered 100% viewport with two masks and a rotated crop: draft, pan, release, settled reuse, history and overlay identity; GPU draw counters must record no blank or stale photo. The first draft is drawn on the GPU's region or by the reference's whole frames; over the GPU's region its capture carries the mask's coverage of that region and the plan's own clipping marks, and its histogram is updating or the reference frame's report of its own revision | `cargo xtask smoke --scenario viewport-region --output NEW_DIR` |
| Rendered ordinary 100% to Fit refit after 1 s of quiet: disable evidence ticks and frame capture before changing view, check requested GPU draw identity and blank count at the deadline before capture resumes, then capture the resulting frame | `cargo xtask smoke --scenario viewport-idle-fit --output NEW_DIR` |
| Rendered Presets: section expand, an XMP and a Luxforge preset imported, each applied from its row, undo, the create form filled and submitted, a native preset applied to the Original, `preset.list` through the `api` step and a delete through the row menu | `cargo xtask smoke --scenario presets --output NEW_DIR` |
| Rendered export over the EXIF orientation 6 fixture, brightened and cropped to 16:9: the title bar's Export menu open, the displayed entry exported with metadata stripped, again keeping it and once through the reference renderer as the palette's Export reference render… does, into the run's evidence directory (only the save dialog is bypassed), each result naming its renderer (the GPU, or the reference for `requested`), the GPU's stripped export decoded within the pointwise display limit of the reference export over every pixel, the two GPU exports of the edited entry (stripped and during comparison) the same bytes, the GPU tile worker's figures showing one stream per GPU export on the window's adapter, each file decoded independently for its dimensions against the captured output stage, its byte length, ICC profile, APP1 segments and EXIF orientation, and a Keep metadata export to the stripped file's name refused with the status bar's reason and the file unchanged | `cargo xtask smoke --scenario export --output NEW_DIR` |
| Rendered Performance section: open and sampling from the launch, a filled window, a straighten, Detail and a commit of all three Presence fields, the stack's reference export listed as long work while it runs and then as finished, collapsed and asleep, then reopened on a fresh window, over the generated 60 MP JPEG, with the editor's memory read by the runner from outside the process and held to the section's figures on idle frames; `--source RAW` runs the same over a RAW photograph, outside `rendered` | `cargo xtask smoke --scenario performance --output NEW_DIR [--source RAW]` |
| Rendered Basic section over a supplied RAW file, in place of a RAW section, which no frame lists: the White balance group's four controls in the JPEG's order, each the RAW development's (Temperature in K and Tint over `set-raw`, the Neutral picker entering the sensor pick, As shot sending `set-raw {white-balance: as-shot}`); a Temperature drag left open and then released, at Fit and at 100%: its first tick planned for the GPU and held only for a reason that passes, its drafted frame drawn on the GPU with no preview job (at Fit over the source reduced to the view, at 100% over the visible region cut from the source) and differing from the frame before it, its histogram marked updating with no report adopted for a drafted frame, and its release landing the exact committed development, drawn at rest with its own report; the moving GPU frame is reported against the release's picture at rest over the photograph on screen by the pointwise statistics and not gated, failing only on a gross error, a worst 16 × 16 block past 10 ΔE00 (owner, 2026-10-06), with the share of the scene's Bayer sites at 0.99 of sensor white or above under the drags' gains recorded beside it as context; each drag keeping the tint in force (the first, from As shot, the core's as-shot tint) in the committed payload and in the Tint field throughout; then a double-click on Temperature, Tint and Exposure: the first press's committed jump and the reset that follows it, each checked as two entries with the reset sent against the jump's revision and never refused — Temperature and Tint back to As shot (`set-raw {white-balance: as-shot}`, the entry labelled Reset White balance, both fields showing the core's as-shot equivalent, checked back through the forward map), Exposure back to 0 EV; Basic's dot, absent on the untouched photograph, present after the committed custom temperature and absent again at As shot and 0 EV; `W` entering the RAW development's sensor pick with Basic's Neutral picker selected, and Escape leaving it; then a crop drafted on the RAW's whole input stage, 16:9 and straightened by 7°, whose draft is one picture (an 80 × 60 grid of stage points is compared with the unstraightened draft at the same points; of the at least 1,200 whose scene is 8 codes or more from the canvas colour there, after the draft's dimming outside the crop rectangle, under 0.5% may show the canvas, so dark scene content the canvas's colour is never taken for a gap; `STRAIGHTENED_RECORDED=DIR cargo test -p xtask straightened_drafts_recorded -- --ignored` judges the straightened draft of each recorded `DIR/raw-panel*` run this way, whatever an earlier check found), applied at Fit, read at 100% through two `render.sample` calls and replaced by a −12° 3:2 `edit.crop-fit` at 100%: no step logs a failure, every committed frame shows the current entry at the output its payload declares, placed and centred at Fit within 4 px, and each sample's codes are the canvas's own at that stage pixel within one code; not in `rendered`, because no RAW photograph is checked in | `cargo xtask smoke --scenario raw-panel --source RAW --output NEW_DIR` |
| Rendered module capabilities: settings, a profile, its key, a download grant and install through `api` steps, the photo-data consent denied then allowed, a task with progress, Apply and a refused task through the desktop, against a loopback proof endpoint | `cargo xtask smoke --scenario capabilities --output NEW_DIR` |
| Rendered UI themes at 1440 × 900: the synthetic Omarchy set (`fixtures/themes/omarchy`) imported through the Appearance tab's Import Omarchy theme… with each theme's outcome listed, a folder imported again listed as already imported; Luxforge Dark, the bundled Nord and both imports drawn, the panels, title bar, a control and the canvas sampled against each frame's recorded tokens, the surround within 0.010 OKLCh chroma, the photograph's pixels and an export's SHA-256 the same under every theme, the Grey canvas kept under a light theme, and a second client switching back with `preferences.set` | `cargo xtask smoke --scenario theme --output NEW_DIR` |
| Rendered Masks panel over the photograph the design boards use: Sky, Face (a radial, a subtracting brush of two strokes and an intersecting luminance range) and Foreground built through the panel and renamed through `mask.rename` and `mask.rename-component`, Foreground's overlay hidden with its eye, Face's amount, Exposure and Clarity through it; then the Brush section armed and put down, a held stroke's draft bar, the New mask menu and Escape, Radial 1's fields, the overlay in each mode and both tints, a hovered row, and Radial 1 selected with its handles resting and no draft, then its rotation grip swung to −12° and committed as one entry on release, the mask-mode board's own state. Every frame's list, open mask, selected component, overlay and mode are checked against the plan, and the draft bar, scope chips, dot and bound layers by state | `cargo xtask smoke --scenario mask-panel --output NEW_DIR` |
| Native masking interaction regressions: unplaced creation, selected/armed targets, live and committed brush flow/feather, deliberate hiding and analytic live-gradient coverage under rotated crop at Fit/100% | `cargo xtask smoke --scenario mask-interactions --output NEW_DIR` |
| Rendered Select workspace over a catalog and folder of images the run generates (`generate-catalog --files 2000 --assets 3000 --images 120`, seed 1; the generated index has no image data behind it, so its cells are placeholders): `G`, with the sources panel's cards, volumes and catalog counts checked against `card.list`, `volume.list` and `catalog.info`; an event opened from the sources panel with its grid grouped by day, camera and moment, the first cell made active and the selection extended with the arrow keys, the Group chip set to Day, an agent's `pick.set` through a second client read again through the event sync; Day › Camera › Moment again, each day heading and moment header counting the picks the layout counts; a single file clicked and picked with `P`, a bracket picked with its header's Pick all, `Cmd+Z` undoing Pick all, then the desktop's pick, then finding nothing of the desktop's to undo while the agent's pick stays, and `Shift+Cmd+Z` redoing the pick, each request checked against what an agent writes with the desktop's actor; a first look at a generated folder of 16,000 files captured with its progress sheet, Continue in background with the job in the status bar and the Performance section, and the job cancelled from its row; the folder of real JPEGs browsed on disk (read by the index lane, then viewed); a scratch folder of six of those JPEGs (`add-folder/`, copied into the run's output) added with Add a folder…, bypassing only the dialog (`index.add-folder` as an agent writes it, its listing followed to its end, the folder then listed under On disk with its six files beside the generated catalog's offline indexed folders, every row as the owner's `index.folders` lists it) and the add undone with `Cmd+Z`, the folder gone from the list and from `index.folders` after the run; then the catalog, after the run develops 28 of the generated JPEGs into the catalog folder `Real photographs` through its own client (`folder.create`, `index.refresh`, `pick.set`, `pick.plan`, `pick.develop`) and makes the library preset `Warm` (`preset.create`: Basic's exposure and a RAW development no JPEG takes): the folder viewed from the sources panel with its photographs' rendered previews, the Metadata browser opened with the core's `browse.facets` counts, a camera chosen in it and a search typed (each changing only its part of the query, the camera's view the size its facet counted), the view saved as a smart collection (`collection.create-smart` with exactly the query shown, read back from `collection.list` after the run) and the smart collection viewed, a photograph clicked and moved to Bodensee (`asset.move` of the selection), five selected and added to Print order from the Info panel (`collection.add`) and the add undone with `Cmd+Z`, and the folder renamed and nested in Travel from its menu (`folder.rename`, `folder.move`); five selected again, `Warm` applied from the Develop band's Apply preset… (`batch.apply-preset` of the selection, its report — five done, each without the RAW development — the `result` of the job's own `job.read` record, the view read again with the five edited) and its report opened from the status bar, and the five exported into the run's empty `batch-export/` folder, answering the folder dialog (`batch.export`, the files on disk exactly those its report wrote); an edited photograph clicked, its Info panel's Send back greyed with a reason that is, word for word, the core's own refusal of each edited photograph's `asset.send-back` asked after the run, and an unedited one right-clicked (selected alone, its menu offering Send back and Remove from catalog…) and sent back from the menu (`asset.send-back` of the selection, the folder's view and All photographs one smaller, the file picked again by the desktop in the catalog left behind); one photograph removed from its Info panel after the confirmation (`asset.remove` of the selection) and the removal undone with `Cmd+Z`, two removed with ⌫ after the confirmation, Removed viewed, one put back (`asset.restore`) and Removed emptied after its confirmation (`catalog.empty-removed`, deleting every removed photograph, the generated ones included), the Removed and All photographs counts at each step those `catalog.info` answered; each request checked against what an agent writes and the catalog left behind read again (one `Preset: Warm` entry in each preset photograph's `history.list`, Removed empty, two photographs fewer); and back to Develop; each frame's `select` block checked against the core's own `browse.view` answers and the owner's `session.state` for the desktop, the folder's moments against its manifest, and the catalog left behind read again for both clients' picks and the journal of their changes | `cargo xtask smoke --scenario select --output NEW_DIR` |
| Rendered Missing originals over originals the run makes and reorganizes (real JPEGs from `generate-catalog --images 120`, seed 1, developed with `pick.develop` from a disk image labelled Photos SSD, a second image, Old SSD, and a scratch card-dump folder; on macOS the images are made with `hdiutil` and attached with `-nobrowse` inside the run, elsewhere scratch folders stand in for them): within Photos SSD one folder moves to `Archive/` with five files as they were, one rewritten, one duplicated and one deleted (its copy kept outside the archive), another goes deep into 20,000 scratch folders, the card dump is deleted and Old SSD detached, and `source.check` records them missing; then `G`, Missing originals from the sources panel, Find in a folder… on the moved folder, the Needs you and All filters, Choose… of the second identical copy, a row selected, Find in a folder… on the deep folder stopped while it walks, Relink, and Locate… of the deleted photograph's copy. Each frame's `missing` block is checked against the core's own `source.missing` and `source.find` answers taken before the launch (`resolve-missing-expected.json`), and the catalog the editor left is read again (`resolve-missing-after.json`): the six verified pairs and the located photograph point at their new files, every other photograph is where it was, and the journal holds exactly the relink and the Locate | `cargo xtask smoke --scenario resolve-missing --output NEW_DIR` |
| Rendered Select loupe over a folder of 120 images the run generates (`generate-catalog --images 120 --assets 1`, seed 1): the folder browsed, a burst's first frame clicked and opened with `E`, `→` twice, `1`, `↓` and `↑` across the moments either side, `Esc`, a bracket from metadata opened and stepped, `Z` (the 100% region at the frame's middle), `C` (the bracket side by side) and `P` picking the bracket's frame where it stands, `Esc`, then the burst's second frame opened and picked with `P`, which moves on to the next moment's first frame; each frame's `select.loupe` block checked against the core's own rows (the active frame's name, the picture its own frame's full loupe tier in every frame, settled, within the decoded budget), the drawn picture's middle and the inset's region read from the capture against the files decoded independently, each bracket frame nearer its own exposure than its siblings', and each `P` one `pick.set` of the active frame's file with the desktop's actor, the view's pick count one higher | `cargo xtask smoke --scenario loupe --output NEW_DIR` |
| Rendered developing picks from a camera card (real JPEGs from `generate-catalog --images 120`, seed 1; the generated card's first Nikon Z 8 folder copied onto a disk image labelled NIKON Z 8, made with `hdiutil` and attached inside the run with `-noautoopen` as a volume a person browses, which Luxforge knows as removable as it does a card (`-nobrowse` would leave it part of the volume it is mounted in), elsewhere a scratch folder; a new catalog with a catalog folder, Portfolio picks, and an indexed folder holding copies of two of the card's first three files): `G`, the card's `DCIM` browsed, its first two files picked with `P`, the third clicked and `D` (picked, and Develop N's confirmation opened), Or add to an existing folder choosing Portfolio picks, a name typed over it, Escape, Develop N again, the name typed again, Develop, the neighbours' large previews decoded, `→` and the photograph's exact render. Every confirmation is checked against the core's own `pick.plan` over a pristine copy (`develop-picks-expected.json`), the Develop's request against what an agent writes for the choice (`use_copies` and `confirm_removable`), its `job.read` record against the catalog the editor left (`develop-picks-after.json`: the two picks with copies point at them, the third at the card, all three in the folder named, no pick left, and the journal holding exactly the desktop's three picks and the Develop), Develop showing the photographs it answered as its set, the `→` frame drawing the next photograph's cached preview in the frame after the key, and each picture's middle against its file's | `cargo xtask smoke --scenario develop-picks --output NEW_DIR` |
| Rendered Develop's filmstrip over a catalog of three generated JPEGs of distinct brightness (`generate-catalog --images 120`, seed 1) and, with `--source`, a copy of a RAW file (the source is hashed before and after; without one the RAW steps are recorded pending, never passed), developed with `pick.develop`: `G`, All photographs, the first photograph clicked and `D` (Develop with the view's photographs as the set), then for each next photograph the look-ahead decoded, `→` captured in the frame after the key, and its exact render; `←`, a press on the first cell, and `Cmd+Option+F` twice. Each frame's set, active photograph, preview identity (asset and entry, drawn) and open photograph are checked against the core's catalog view (`filmstrip-expected.json`), and each picture's middle against the photograph's own large preview read from the cache after the run (`filmstrip-after.json`) and nearer it than any other's | `cargo xtask smoke --scenario filmstrip --output NEW_DIR [--source RAW]` |
| The capability framework's own costs (registration, capability reads, a task, artifact publish, cancellation), release only | `cargo test --release --locked -p luxforge-core --lib capability_timing -- --ignored --nocapture` |
| How promptly a cancelled 24 MP render stops, in the transform pass and mid colour chunk, against its 25 ms bound, release only | `cargo test --release --locked -p luxforge-core --test cancellation -- --ignored --nocapture cancelled` |
| Inspect a capture | `cargo xtask check-capture --image PNG [--orientation N]` |
| How far one frame is from another over the photograph alone: mean, worst 16 × 16 block, p99 and signed mean ΔL\* in CIEDE2000, with the pointwise and spatial verdicts of the [GPU preview limits](../design/gpu-preview.md#the-preview-error-limit), as JSON. Two PNGs and the photograph's rectangle, or two frames of an evidence run, whose recorded rectangle it reads; `--class` makes a miss a failure. Nothing is launched | `cargo xtask preview-error --candidate PNG --reference PNG --photo-rect LEFT,TOP,RIGHT,BOTTOM [--class pointwise\|spatial] [--output NEW_FILE]`, or `--evidence DIR --candidate-frame N --reference-frame N` in place of the first three |
| The GPU qualification corpus, `fixtures/preview/corpus.json`: every source and recipe, each at Fit, 33%, 50% and 100%, checked against the committed descriptors, each source re-hashed on this host and each RAW entry resolved through the private RAW manifest. A source it could not check here makes the run incomplete (exit 3), never a pass | `cargo xtask preview-corpus [--manifest FILE] [--output NEW_FILE]` |
| The release gate of the GPU-first renderer ([image correctness](#image-correctness-exact-and-reference-buffers)): every stack of the corpus rendered on the reference renderer and on the GPU at Fit, 33%, 50% and 100%, each output kind the GPU renders held to its recorded limit ([GPU-first](../design/gpu-first.md#the-contract)). The picture is two kinds, each drawn by the photo surface's own drawing on a headless device from the photograph's source held on the GPU as the editor holds it: at rest, the editor's GPU picture of the stack (its tiles at full resolution reduced to the view at Fit, 33% and 50%, its view plan over the visible window at 100%), gated against the reference frame reduced to the view's size by an independent linear-light reduction (its visible region at 100%); in motion, the frame a drag draws over the boundary the surface derives from the source, reported against the picture at rest it settles to and against the reference with the same statistics and limits but not gated, since what a drag's frame is held to is an open owner question ([GPU-first](../design/gpu-first.md#proposals-with-recorded-defaults)). `--gate-motion against-rest` or `--gate-motion against-reference` gates it, so a run shows its misses as failures. Export is judged over the whole output stage: each stack streamed in tiles by the desktop's GPU tile worker, as the export lane streams it, after the reference frame's render has stored its estimates, its codes against the reference export's by the display limit of the stack's class, and a second stream on a second device the same bytes; a stack the GPU cannot export there is a gap naming why. The histogram and clipping counts are judged over the whole output stage too: each stack's tiles at full resolution, as a committed job plans them, drawn by the photo surface's own drawing for their counts alone and counted by its histogram reduction, against the core's reducer over the reference frame, by the earth mover's distance within a quarter of a code on each of R, G, B and luminance, the luminance histograms from the tiles read back whole, and each clipping counter within 0.1% of the output pixel count; the summed bin difference, the bin differences at 2 and 4 codes, the shares of pixels one, two and more codes from the reference frame's and, for a stack past a limit, both sides' bins are reported beside them; a stack whose tiles the GPU cannot draw, or whose tiles read back are not the frame it counted, is a gap naming why. Samples are judged at a 5 × 5 grid of points over the output stage: each stack's points read by the first tile worker as `render.sample` reads them, one call, against the reference frame's bytes there by the display limit of the stack's class and against the byte the picture at rest shows there at 100%, drawn over a window around the point, which each must equal; a point the reference answers is a gap naming why. It builds the desktop crate's harness in release, launches no editor, re-hashes every source after the run and writes `report.json`, `summary.md` and the frames of every judged cell past a limit (`--frames all` for every cell). A judged cell past a limit fails it; a cell that could not run, a host without an adapter or a selection (`--zoom`, `--kind`, `--families`, `--recipes`, `--sources`) makes it incomplete (exit 3), never a pass | `cargo run --release --locked --package xtask -- gpu-qualification --output NEW_DIR [--manifest FILE] [--fixtures DIR] [--zoom fit\|33\|50\|100\|all] [--kind picture-at-rest\|picture-in-motion\|histogram\|sample\|export\|all] [--families F,...] [--recipes ID,...] [--sources ID,...] [--frames missed\|all] [--gate-motion against-rest\|against-reference]` |
| The GPU colour programs on this host's device, each against its CPU unit over a dense synthetic grid by the pointwise limits and the finiteness rule, every shipped program against the photo surface's own prelude, the plan conversion, and the precision of the transcendentals the programs use. Without an adapter each device test prints that it was skipped | `cargo test -p luxforge-app gpu_colour -- --nocapture` |
| The GPU Presence program on this host's device: every kernel against the CPU filter it transcribes on synthetic planes (box means at several run lengths, minima, both guided filters, the reductions, the upsample, the soft clip and the atmospheric light), every Presence combination against the CPU frame by the spatial limits, and the program against the photo surface's own spatial convention. Without an adapter each device test prints that it was skipped | `cargo test -p luxforge-app gpu_presence -- --nocapture` |
| The GPU Detail program on this host's device: every kernel against the CPU kernel it transcribes on synthetic planes (the Oklab conversion, the B3 and Gaussian smoothing, sharpening's blur and guide, one level's shrinkage, sharpening's change of lightness and the reconstruction), the precision of `tanh`, both units against the CPU's at full resolution and at a proxy's scale by the spatial limits, the shared pass pipelines, the budget's charge at Fit, and the program against the photo surface's own spatial convention. Without an adapter each device test prints that it was skipped | `cargo test -p luxforge-app gpu_detail -- --nocapture` |
| The GPU mask coverage programs on this host's device: each kind's coverage, read back from the photo surface's masked step, against its CPU field by the half-coverage contour rule at Fit and exact, finiteness over the extreme grid, a stroke painted over 200 ticks, every coverage program against the surface's own prelude and the masked plan conversion. Without an adapter each device test prints that it was skipped | `cargo test -p luxforge-app gpu_mask -- --nocapture` |
| A program class's families of the GPU preview corpus alone, every GPU frame against the reference frame at the view's size by its recipe's class: the release gate's selection, which is incomplete (exit 3) because it is not the whole corpus ([performance](../specs/performance.md#gpu-qualification-against-the-reference)) | `cargo run --release --locked --package xtask -- gpu-qualification --output NEW_DIR [--manifest FILE] --families F,... [--zoom fit\|33\|50\|100]`, the colour programs' families being `basic,tone-curve,mixer,vignette,colour-stack,crop,lens-perspective`, the masks' `mask-linear,mask-radial,mask-brush,mask-luminance-range,mask-colour-range,mask-composed`, and `presence` and `detail` |
| The per-frame light's reduction factor: for every Dehaze stack of the measurement's cells, at 100% in the largest window the M4's display holds and at Fit behind the corpus's straightened crop, the GPU frame drawn with each candidate light — emulated on the CPU at a block stage reduced by 16, 8, 4, 2 and 1 per side, with and without Detail, and the reference's own — handed to the plan's light planes, against the reference frame of the view by the spatial limits, beside each light's error ([performance](../specs/performance.md#the-per-frame-lights-reduction-factor)); `LUXFORGE_GPU_CORPUS_SOURCES=ID,...`, `LUXFORGE_DEHAZE_DRAGS=ID,...` and `LUXFORGE_DEHAZE_VIEWS=100%,fit` run only those | `LUXFORGE_GPU_CORPUS_OUTPUT=NEW_DIR LUXFORGE_GENERATED_FIXTURES=DIR [LUXFORGE_RAW_MANIFEST=FILE] cargo test --release -p luxforge-app gpu_dehaze_reduction_factor_candidates -- --ignored --nocapture` |
| Process failure checks; macOS measurement, `--samples` defaults to 5 launches per workload | `cargo xtask hardening --binary PATH --output NEW_DIR`, `cargo xtask measure --binary PATH --output NEW_DIR [--samples N]` |
| The catalog's provisional performance targets in one report, `catalog-measure.json` with `catalog-measure.md` beside it: every row with its p50/p95, scope, cache state and the load at its step's start and end, and a `not_measured`, `skipped` or `failed` row with its reason for anything not taken. Its data is made under the output's `scratch/`: generated files, photographs and JPEGs, a tree and a folder of copies of those JPEGs, a tree of hard links to them, and a RAW trip copied from the corpus (`--raw-corpus`, else `LUXFORGE_RAW_CORPUS_DIR`; its rows are skipped when it is absent). It times `browse.view` and `browse.rows` at the design's scale, the first browse of the trip as an indexed folder (the first screen, every file, event and moment, every grid preview), returning to it and developing its picks, a first index of the tree of copies and of the tree of hard links (200,000 links at full scale, 400 sharing each JPEG's file identity; its rows skipped where the file system refuses hard links), each with the owner's round trips sampled throughout, the preview bracket check per run (the core's ignored bench `bracket_probe_per_run`), idle CPU with the watchers armed in the core and in the editor, and a Basic drag through `editor-latency` alone, during indexing and during a preview backlog. `--card PATH` browses the mounted camera card that path is on, as a card, for a first browse from a card reader; without it those rows are skipped. `--samples` defaults to 30, and a first browse takes at most 5. `--scale tiny` proves the harness in minutes and claims nothing. Its desktop step (`xtask/src/catalog_probes/`) times, through background evidence launches of the editor, grid scroll over the folder (the updates that adopt each offset), loupe stepping with the look-ahead warm (in display frames of the observed frame interval, and in ms) and a held arrow at 30 ms, the 100% focus check from a full-size embedded preview and, over the trip, from a development per camera (skipped without it), and the decoded grid and loupe previews the editor held meanwhile, each probe run on its own: one that fails is one `failed` row of its own (`desktop.probe.<run>`) naming its run directory, beside the other runs' rows; the Develop switch is a row it reports `not_measured` until it is built. Not part of `verify`'s timing tier | `cargo run --release --locked --package xtask -- catalog-measure --output NEW_DIR [--samples N] [--scale tiny\|full] [--only STEP] [--binary PATH] [--raw-corpus DIR] [--card DIR]` |
| Package; dependency inventory | `cargo xtask package --output NEW_DIR`, `cargo xtask inventory --output NEW_DIR` |
| License, source and advisory policy | `cargo xtask audit`, see [dependencies](dependencies.md) |

Every evidence command refuses an existing output directory: use a fresh `artifacts/<run-id>/`. Default sample counts are functional runs: they prove the journey and give one launch count to quote, not a distribution. A p50/p95 claim needs the explicit counts stated in the [performance plan](../specs/performance.md#sample-counts-for-a-p50p95-claim). Timing commands must use release builds. A [debug build](#test-and-debug-builds) is only lightly optimized, which is why `develop` defaults to release. `check` never implies graphical or dependency-audit acceptance.

### Repository rules

`cargo xtask check-repository` validates the task plans and local links, then applies three rule tables in `xtask/src/repository.rs`. Each row is one rule; a refusal names the file and line, the rule, the token, dependency or message it found, and the table whose row to change.

`SOURCE_RULES` says which files may hold which tokens. A row names its tokens, its scope (directories and file types, since `crates/luxforge-raw/vendor` holds LibRaw's C++ sources), the paths allowed to hold them (a file, a directory, or a module path such as `modules/raw` for `raw.rs` and `raw/`), its match mode, whether it covers test code, whether each allowed path may hold each token on one line only (an expression written once in its home; each file under an allowed directory is a home of its own), and the reason it prints. One matcher serves every row. `Whole` checks an end of the token only where the token has an identifier character there, so `app::` finds `crate::app::State` but not `snapp::`, and `RAW_EFFECT` misses `RAW_EFFECTS`. `Prefix` lets the token run on into a longer identifier, so `mozjpeg` finds `mozjpeg_sys`. A row that covers tests reads every line, comments included. A row that does not skips files that are tests by name (a `tests` directory, `tests.rs`, `*_tests.rs`), modules declared under `#[cfg(test)]` with everything under their directory, `#[cfg(test)]` items, and comment lines. No source rule reads `xtask/src/repository.rs`, which names every token in its rows and tests.

| Rule | Refuses | Where |
| --- | --- | --- |
| `state-layer`, `view-layer`, `widget-crate` | Iced, `luxforge_ui`, `view::` and `app::` in the view model; the core, `OwnerHandle` and `.call(` in the view, its canvases included; the core in `luxforge-ui` | Those directories, tests included |
| `jpeg-codec-name`, `jpeg-through-codec` | `mozjpeg` outside `luxforge-jpeg`; decoding JPEG through `image` | Shipped crates' production code |
| `raw-identity` | `"luxforge.raw"` and `RAW_EFFECT` outside the RAW module | Production code under `crates/` |
| `presettable-action` | The refusal `is not a field-patch action` outside `ModuleRegistry::patch_action`, the one answer to whether an action is presettable | Production code under `crates/` |
| `refusal-text` | The unavailable-effect and full-source-queue messages (`"unavailable effect`, `queue is full`) outside their constructors' homes (`error.rs`, and `api/owner.rs` for the two queues): a client, the desktop included, reads the refusal's code and data (`data.effect_id`, `data.retry`), never its message | Production code under `crates/` |
| `component-kind` | A mask component kind's token (`BRUSH`, `LINEAR`, `RADIAL`, `KIND`, `LUMINANCE_KIND`, `COLOUR_KIND`, `"luminance-range"`, `"colour-range"`) outside the host's kind table (`mask/mod.rs`), each kind's own file and the desktop's drawn-kind table and editors | Production code under `crates/` |
| `one-read-rectangle` | A resample's tap index, `- 0.5).floor()`, anywhere but once, in `Resample::reads` in `render/geometry.rs` | Core production code |
| `cpu-proxy` | The CPU proxy's items (`cpu_proxy::`, `CpuProxy`, `ProxyPhase`, `ProxyCache`, `ProxyKey`, `ProxyStage`, `ProxyOutcome`, `ProxyApproximation`, `ProxyFrame`, `PhaseOutcome::Proxy`, `PreviewIntent::Interactive`, `render_proxy`, `proxy_stage`, `proxy_eligible`, `proxy_cancellable`, `whole_within`, `cpu_proxy_tick`, `cpu_proxy_bounds`) outside the core's `cpu_proxy` and the desktop's `app/cpu_proxy`, and their dispatch sites: the preview worker, queue and result, the crate root's re-exports, and the desktop's `motion.rs` and `preview.rs` | Production code under `crates/` and `xtask/src` |
| `patch-action` | A patch action's declaration (`patch: true`) outside the field-patch module and the RAW module, whose `set-raw` keeps its own merge | Production code under `crates/` |
| `job-records` | A ring of finished job records, `VecDeque<JobId>`, outside the one job table (`jobs.rs`) | Core production code |
| `job-table` | `Jobs::new` anywhere but once, in the catalog owner's launch (`api/owner.rs`) | Core production code |
| `one-envelope-check` | `mutation.validate()` outside the dispatcher's one envelope check (`Envelope::check` in `api/params.rs`), which checks every mutating method's envelope before any handler runs | Core production code |
| `one-source-preparation` | A synchronous service mode (`allow_sync_source`) and the reads of an original or an artifact (`prepare_file`, `read_bounded_file`, `read_verified`) outside the source work in `editor/source.rs` (`SourceWork::run`, which the owner's source worker and the blocking helpers `EditorService::import` and `EditorService::prepare` share) and the reads' own homes (`source.rs`, `artifacts/`, the re-export in `lib.rs`) | Core code, tests included |
| `desktop-crop-rows` | `CROP_EFFECT`, `ORIENTATION_EFFECT`, `from_value::<CropPayload>` and `from_value::<Orientation>` anywhere in the desktop: it reads a crop, its stage and the orientation ahead of it from `recipe.describe` rows | Desktop production code (`crates/luxforge-app/src`) |
| `declared-crop-angle` | `MIN_ANGLE` and `MAX_ANGLE` outside the frame's geometry (`crop_draft.rs`): the angle's control reads its range, steps and default from the declared parameter | Desktop production code (`crates/luxforge-app/src`) |
| `desktop-start-refusal` | `"Return to the current state before` and `"Waiting for the last request` anywhere in the desktop but once each, as the constants beside `state::edit_refusal` in `state/mod.rs`: a start's refusal is `Editor::gesture_refusal`, whose editable half is the view model's one editability rule (`state::editable_refusal`), and a start site or a model writes the reason it is given | Desktop production code (`crates/luxforge-app/src`) |
| `desktop-group-reset` | `resolve_group_reset` outside the tools panel model (`state/tools.rs`): `ResetGroup` runs the reset the section resolved for the photo and target (`SectionModel::group_reset`) | Desktop production code (`crates/luxforge-app/src`) |
| `evidence-outcomes` | `Settle::`, `settle_step`, `await_step`, `refuse_step` and `capture_next_frame` outside the evidence driver (`app/evidence.rs`): a seam reports what happened as a typed outcome through `Editor::outcome` (`app/outcome.rs`), and only the driver names what a script step waits for and settles, arms or refuses it | Desktop production code (`crates/luxforge-app/src`) |
| `desktop-keeps-no-stack` | `Evaluation` anywhere in the desktop but once, as the thumbnail and live-coverage workers' job types in `app/thumbnails.rs` and `app/mask_coverage.rs`: an evaluation holds its source, and a RAW development's planes hold the source worker's memory gate, so one kept between messages would stop the next development from starting | Desktop production code (`crates/luxforge-app/src`) |
| `desktop-keeps-no-preview-job` | `PreviewJob` outside the files that pass one straight through: the message files (`app/message.rs` and each seam's `app/message/<variant>.rs`), the owner tasks (`app/tasks.rs`), the preview request (`app/preview.rs`), and a `draft.set`'s and the thumbnails' answers (`app/gesture.rs`, `app/thumbnails.rs`, `app/mask_coverage.rs`). A planned job holds its stack, so the crop draft, the editor and the view model keep frames and identities only; a token rule cannot see a job kept inside one of those files or inside a carrier type (`Refresh`, `PreviewPayload`) | Desktop production code (`crates/luxforge-app/src`) |
| `render-limits-home` | The one-megapixel parallel threshold or the 512 MiB frame limit's literal assignment, `= 1_000_000;` or `= 512 * 1024 * 1024;`, outside `luxforge-raw/src/limits.rs` | `luxforge-core` and `luxforge-raw` production code |
| `draft-preview-rule` | `RawSettingsMode::DraftPreview`, the drafted preview's approximate white balance, outside the one evaluation builder (`editor/evaluate.rs`) and the RAW settings resolver (`editor/source.rs`) | Core production code |
| `test-waits` | `sleep(` and `yield_now`: a test's own sleep, spin or poll loop, outside `luxforge-testbase`'s one wait. The widget crate's GPU retirement worker (`photo_surface.rs`) keeps its one production sleep, on one line, so the tests beside it are held to the rule too | `crates/`, tests and comments included |
| `test-gates` | `Condvar`: a test's own gate, outside `luxforge-testbase`'s `Gate` and the core's production blocking points (the source worker's plane gate in `source.rs`, the latest-job worker in `latest.rs`, the reference tile worker in `tiles/reference.rs` and the 100% focus check's one RAW development at a time in `previews/region.rs`), the desktop's mask-coverage handoff and its GPU tile worker | `crates/`, tests and comments included |
| `one-distribution` | A second percentile definition outside `luxforge-testbase`'s `Distribution` (`distribution.rs`): the shapes a hand-written one took (`fn percentile`, `let percentile`, `fn median`, `let median`, `fn p50`, `let p50`, `let p95`) and a nearest-rank rank computed again (`div_ceil(100)`) | `crates/` and `xtask/`, tests and comments included |
| `thread-spawn` | `thread::spawn`, `thread::Builder` and `thread::scope` outside the declared worker homes: the core's source worker and owner loop, API transport threads, the job table's lanes, latest-job worker and reference tile worker, and the catalog's index, preview and library lanes; the folder watcher's thread on Linux and Windows; the desktop's diagnostics log writer and GPU tile worker; the widget crate's GPU retirement worker; the test kit's process and server threads; `verify`'s component pool | Production code under `crates/` and `xtask/` |
| `one-photo-locator` | `["photo_rect"]`: a scenario reading the rectangle the editor records drawing the photograph in for itself, outside the one locator over it (`Frame::photo_rect`, `photo`, `visible_photo`, `photo_edges` in `xtask/src/scenario/pixels.rs`) | `xtask/src/`, tests included |
| `editor-launch` | An editor argument (`"--evidence-dir"`, `"--evidence-script"`, `"--data-root"`, `"--catalog"`, `"--open"`, `"--developer"`, `"--disable-module"`, `"--proof-endpoint"`, `"--window-size"`, `"--evidence-gpu-identity"`, `"--no-gpu-render"`, `"--gpu-adapters"`), `spawn_editor` or `editor_args` outside the scenario library's launch envelope (`xtask/src/scenario/launch.rs`) and the hidden-window flag's home (`xtask/src/launch.rs`), which also runs doctor's `--gpu-adapters` | Production code under `xtask/` |
| `registry-assembly` | `register_unavailable(`, `PixelModule::new(`, `ControlsModule::new(` and `CapabilitiesProofModule::new(` outside the one assembly, `ModuleRegistry::assemble` (`crates/luxforge-core/src/modules/registry/mod.rs`), which the desktop, `luxforge-json` and the harness all call, so test modules join only a developer run | Production code under `crates/` and `xtask/` |
| `raw-manifest-reader` | A RAW manifest read outside the one reader, `raw::manifest` (`xtask/src/raw.rs`): its list named untyped (`["sources"]`) or its fields declared again (`neutral_point:`). `verify` and the `raw-editor` scenario read their manifests through it | Production code under `xtask/` |
| `http-framing` | `ureq_proto::` and `httparse::`, the HTTP/1.1 framing under `ureq`, outside the module transport (`crates/luxforge-net`) | `crates/` and `xtask/`, tests included |
| `no-pixel-image-handle` | `Handle::from_rgba`, which uploads a new texture each time it is made | `crates/` and `xtask/`, tests included |
| `project-name` | The old working name | Every text file under `crates/` and `xtask/` |
| `srgb-transfer-function` | `12.92`, the sRGB transfer function's linear-branch slope, outside the core's production copy (`crates/luxforge-core/src/colour.rs`) and the one shared test reference (`luxforge_reference::srgb`); a decode/encode transcription is always found by its slope, whichever spelling of the threshold (`0.04045`/`0.0031308` or `0.040_45`/`0.003_130_8`) it uses | `crates/` and `xtask/`, tests included |

`DEPENDENCY_RULES` says which crate may depend on what. A row names the dependencies it refuses (a crate, any of a list of crates, a name prefix, a crate with a feature, or any workspace crate: a `luxforge` name or any path), the manifests it reads (`""` for the workspace root, `crates/*` for every crate), the tables it reads (normal, dev, build or `workspace.dependencies`; a target-specific table counts as the table it names), the crates allowed to declare them, and its reason. A dependency is known by its key and by the package it renames, in any spelling: a key line, a dotted key, an inline table, a multi-line value or its own `[dependencies.name]` table.

| Rule | Refuses |
| --- | --- |
| `image-jpeg-feature`, `workspace-image-jpeg` | `image`'s `jpeg` feature in a shipped crate's normal dependencies or in `workspace.dependencies`; tests and tools add it for themselves |
| `mozjpeg-links` | A `mozjpeg*` dependency of a shipped crate other than `luxforge-jpeg` |
| `jpeg-codec-users`, `jpeg-codec-leaf` | A dependency on `luxforge-jpeg` from any crate but `luxforge-core`; any workspace crate or path in `luxforge-jpeg`'s manifest |
| `independent-references` | Any workspace crate or path in `luxforge-reference`'s manifest |
| `core-free-test-base` | Any workspace crate or path in `luxforge-testbase`'s manifest, so the core's and the widget crate's tests can use its gate, wait and distribution |
| `http-client-crates` | A dependency on `ureq` or `ureq-proto` from any crate but `luxforge-net`, whose module transport is the one HTTP client |
| `core-links-no-network` | `rustls`, `rustls-platform-verifier`, `ring`, `ureq`, `ureq-proto` or `security-framework` in `luxforge-core`'s normal, dev or build dependencies: the transport and the Keychain store are `luxforge-net`'s, given to the host through `HostConfig`, so the core and its test binaries link neither |
| `headless-cli` | A normal dependency of `luxforge-cli` on the GUI stack (`iced`, `iced_wgpu`, `wgpu`, `naga`, `rfd`, `luxforge-ui` or `luxforge-app`), so building the headless `luxforge-json` builds no window, renderer, shader compiler or dialog crate |
| `core-qualification-only-in-tests` | `luxforge-core`'s `qualification` feature turned on by a normal, build or workspace dependency: the CPU filters it exposes (`luxforge_core::qualification`) are for the desktop's GPU readback tests, so only a `[dev-dependencies]` table turns it on |
| `allocation-counter-only-in-tests` | `luxforge-process`'s `allocation-counter` feature turned on by a normal, build or workspace dependency: the counting allocator it exposes (`luxforge_process::allocations`) is for test binaries that hold a path to an allocation budget, so only a `[dev-dependencies]` table turns it on |
| `gpu-free-core` | A normal or build dependency of `luxforge-core` on a GPU or GUI crate (`iced`, `iced_wgpu`, `wgpu`, `wgpu-core`, `wgpu-hal`, `wgpu-types`, `naga`, `rfd` or `luxforge-ui`): a module's GPU program is WGSL text the photo surface executes, and the core's tests may validate it with `naga` as a dev-dependency |

`SENDER_RULES` says which messages product code must send. A row names a message enum and the file that declares it, the file whose `match` handles it, the directories read for senders and the scripted drivers whose constructions do not count. Every variant needs a sender: a production line (tests skipped as for a source rule) outside the drivers that holds `Enum::Variant` under the `Whole` matcher. In the handler, a line that begins with a variant is its match arm, not a sender; elsewhere rustfmt may begin a line with a construction, and it counts.

| Rule | Refuses |
| --- | --- |
| `widget-sent-controls` | A `ControlMessage` variant (`app/message/control.rs`) that only the evidence driver (`app/evidence.rs`) or a test constructs: evidence and tests drive a control through the message its widget sends, such as a slider's `Fraction` and `Released` |

The check also refuses a stale row: one that reads no file, names a path that does not exist, or shares another row's name.

A task that finishes a concept adds the rule that keeps it single as a row of one of these tables, with a test that shows an allowed and a refused path; it never writes a bespoke check. A deliberate new home for something a rule confines is a change to that row's allowed paths, made in review.

### When to verify

Verification is for finished work. Several agent sessions share the owner's M4, and a whole-workspace
test run, a rendered tier or a timing run occupies most of its cores while it runs and skews any
timing run elsewhere on the host. Run each check where its answer can change what happens next:

| Stage | Run |
| --- | --- |
| Editing | The tests for the code you are changing: `cargo test -p CRATE FILTER`, narrowed to one binary with `--lib` or `--test NAME`. For a rendered change, the one smoke scenario that covers it, against a current release build. Nothing whole-suite and nothing timed. |
| Change complete: code, tests and docs | `verify --tier quick`, once, before handing off. After a failure, fix it and rerun only what failed (the named test or the one scenario), then `quick` once more. |
| Integration point | `rendered` for a change touching rendering or the UI; `timing` for a change under `crates/` that can affect cost. |
| Milestone claim | `full`, with `--manifest`. |
| Release | `gpu-qualification` with `--manifest`, the release gate: `rendered` and `full` run it after their scenarios. |

Timing runs wait until feature work is complete: the `timing` tier, `editor-performance`,
`editor-latency`, `detail-performance`, `detail-grid-performance`, `measure`, the drag-tick benchmark
([software adapters](../specs/performance.md#software-adapters)) and every `--samples 30` distribution. A figure taken
mid-implementation measures code that is about to change, on a host loaded by builds and tests. The
exception is work whose subject is performance (a budget, a regression, an optimization), where
measurement is the feedback: iterate with the one targeted command at its default sample count, and
take the claimed distribution and the before/after once, at the end.

While changing what the GPU draws, iterate on `gpu-qualification` with a selection (`--families`,
`--recipes`, `--sources`, `--zoom`) of the stacks the change touches; a selection is incomplete, never
the gate's pass, and the gate itself runs once, with `--manifest`, on the integrated branch.

When work is split across agents, each agent runs targeted tests while working and `quick` at
hand-off; the integrator runs `rendered`, `timing` and `full` once, on the integrated branch.

### Image correctness: exact and reference buffers

The GPU is the renderer of record and the CPU's `luxforge_core::render` is the reference renderer
([GPU-first](../design/gpu-first.md)), so a claim about pixels rests on one of two kinds of test:

- **Exact-buffer tests** hold the reference renderer, and every CPU path that still supplies an
  output (the CPU proxy of a session without a GPU, the reference histogram, samples and export),
  to frozen fixtures and to the
  independent `f64` references in `luxforge-reference`, byte for byte or within the precision that
  crate declares. They are ordinary tests, so `check` and hosted CI run them.
- **Reference-buffer tests** hold what the GPU renders to what the reference renderer computes,
  within a declared tolerance: each program against its CPU unit on synthetic grids (the `gpu_*`
  tests in the commands above), which is what enables a program, and every corpus stack at every view through `gpu-qualification`, the release
  gate, by its output kind's tolerance (`luxforge_reference::tolerance`): the picture at rest
  against the reference frame at the view's size, gated, and a drag's frame against the picture at
  rest it settles to and against the reference, reported and not gated while the owner's question
  stands. Bit identity is never asked of the GPU: on one machine and driver its frames are
  identical frame to frame, and across machines the last digit may differ.

A kind the GPU does not render yet is reported as such by the gate, neither a pass nor a failure,
and the reference renderer's exact-buffer tests stand for it. A host without a GPU adapter proves no
tolerance: each `gpu_*` test prints that it was skipped and the gate is incomplete.

### Test and debug builds

The `dev` profile, which `cargo test`, `cargo xtask` and `develop --debug` build with, compiles at
opt-level 1 with debug assertions and overflow checks on, and builds dependencies without debug
info. The pixel tests are the reason. Back to back on the owner's M4, the five slowest test binaries
took 139 to 150 s at opt-level 0 and 17 s at opt-level 1. A new worktree's first test build grew
from 56 s to 97 s, and a rebuild after a one-function edit in `luxforge-core` by one to five
seconds. Building dependencies at opt-level 3 saved no test time and cost another 21 to 23 s of
first build. To step through code in a debugger, build that once with `--config profile.dev.opt-level=0`.
A dev build is not a timing build; timing uses release.

Every integration-test binary links the whole of `luxforge-core`, so the core's integration tests
are grouped into one binary per area, one module per file: `basic` (Exposure, white balance, Tone,
Colour), `modules` (the mixer, Presence, the vignette, the controls proof, presets and the
field-patch conformance suite), `mask` (the kind-conformance suite, whose one checklist every
component kind passes against its reference through one adapter per kind, each kind's own tests,
the command family through the JSON method table, the coverage grid, geometry survival and masked
edits on both paths), and `cancellation`, which holds the ignored release timings and stays apart
so they never share a process with the pixel tests: how promptly a cancelled 24 MP render stops,
against its 25 ms bound (filter `cancelled`), and the cost of `resources.read` (filter
`resources_cost`, run alone so its first half runs in a process that has touched no GPU). Narrow a
run with the module path, for example `cargo test -p luxforge-core --test basic white_balance::` or
`--test mask range::`. `luxforge-cli`'s process tests are one binary, `json_cli`, one module per
slice: `process`, `presets` and the ignored authentic RAW journeys in `raw`
(`cargo test -p luxforge-cli --test json_cli presets::`).

Test helpers are split by whether they name a core type. `luxforge-testbase` holds what needs none:
the gate, the wait and the distribution, the loopback `TestServer`, the proof endpoint
`ProofEndpoint` and the fixture and scratch paths (`paths`). `luxforge-testkit` holds what does:
`client`, `fixtures`, `JsonProcess` and `proof_protocol`. `luxforge-core` names only the base, so
testing the core compiles it once: a dev-dependency on the kit, which depends on the core, would
compile the core a second time for every core test build, `--lib` included. On the owner's M4,
touching one core source file and rebuilding `cargo test -p luxforge-core --lib --no-run` took 4.6
to 7.0 s with that second build and 2.7 to 3.3 s without it (three runs each, one-minute load 5 to
11 on a shared host). The core's integration binaries compile the kit's `client.rs` and
`fixtures.rs` in through `#[path]` modules and name themselves `luxforge_testkit`
(`extern crate self as luxforge_testkit;` in each `main.rs`), so a helper call reads the same in
every crate's tests; a helper those binaries need goes in those files, and one the core's unit
tests need goes in the core's own test-support modules (`editor/test_support.rs`,
`capabilities/testing.rs`, `artifacts/testing.rs`). The independent references and their studies,
the mask, brush and range studies among them, live in `luxforge-reference`, one module per study
(`cargo test -p luxforge-reference --test studies tone::`).

### How `check` runs the tests

`check` runs the repository and dependency-policy checks and formatting beside Clippy and the tests,
since they share nothing with the build. `check` and `test` build every test binary through one
`cargo test --no-run`, then run all of them at once, each as its own process in its package's
directory as Cargo runs it, and print one line per binary with its elapsed time and a failing
binary's whole output after the rest. `cargo test` runs its binaries one after another, so a
workspace run took the sum of every binary's time; the slowest binary now bounds it. The doctests
run last, through Cargo.

A test whose own name starts with `slow_` is a slow test: one that takes about a second or more on
its own in the dev profile, such as an exhaustive sweep, or a spatial layer's whole tile evaluated
again for each sampled point. `check --quick` and `test --quick` list each binary's tests, skip the
slow ones by exact name, leave out the doctests and say how many tests they left out; without
`--quick` nothing is left out. The quick tier runs `check --quick`, and every other tier and CI the
whole `check`. Name a new test `slow_` only when it alone would lengthen the quick run, after making
it as cheap as what it proves allows. A slow test runs by hand like any other, for example
`cargo test -p luxforge-core --lib spatial::tests::slow_`. The field-patch conformance test is one:
`editor-acceptance` runs the same suite in release in a quarter of the time.

### Tests skip the disk flush

Every durable write in the core, the catalog's commits aside, makes its bytes durable through one
function, `atomic_file::flush`: `sync_all`, which on macOS is `F_FULLFSYNC`, a flush of the drive's
own cache that the whole host queues for. The catalog commits with `synchronous=FULL`. Test builds
skip both through `luxforge-core`'s `test-skip-disk-flush` feature (the catalog commits with
`synchronous=OFF`); every other step of a durable write, the temporary file, the rename and the
locks, still runs, and no test can observe a flush. With the flush, `luxforge-core`'s unit tests
took 3.8 s on their own while using about three cores, waiting on the drive; without it, 1.8 to
2.0 s.

Only `[dev-dependencies]` tables turn the feature on: every crate that depends on the core names it
again there with the feature, the core itself included. Cargo turns on a dev-dependency's features
only when it builds tests, so `cargo build`, `cargo xtask develop`, `build --release`, `package` and
`verify`'s release build never have it, while `cargo test` and `clippy --all-targets` do, and so does
anything they link in the same run: `target/debug/luxforge-json`, which the CLI's integration tests
run, is the flush-skipping build until something else rebuilds it. `check-repository` holds this with
two rules: `disk-flush-only-in-tests` refuses the feature in any normal, build or workspace
dependency, and `one-disk-flush` refuses `sync_all`, `sync_data` and the feature's name anywhere in
the crates' production code outside `atomic_file.rs`.

The cost is one more compile of the core after a core edit: `cargo xtask` builds `xtask` against the
core without the feature and the tests build it with the feature, where before they shared one. On
the owner's M4 a one-line edit recompiles the core incrementally in about 2 s.

### Tests that do not depend on host load

The workspace's tests run in parallel, beside other builds and test runs on a shared host, so a test
passes or fails the same way however loaded that host is:

- **No shared mutable state between tests.** A test reads only figures it owns: its own render
  context, registry, catalog, queue or pipeline, never a process-wide counter another test also
  moves.
- **Order comes from gates and channels, never from sleeping for long enough.** Work a test needs
  held — a render, a job, a test server's answer — passes a
  `luxforge_testbase::Gate` the test shut; the test waits for the work to reach it
  (`Gate::wait_reached`), acts while it is held, then opens it. What a test cannot be told about,
  such as a job status read through the API, it polls through `luxforge_testbase::wait_until` or
  `wait_for`.
- **A deadline only bounds a hang.** Every wait fails naming what never happened after the one hang
  bound, `luxforge_testbase::HANG` (two minutes); no test outside the timing tier asserts how fast
  anything happened.

`luxforge-testbase` holds the one gate and the one wait. It depends on no workspace crate, so the
core's own unit tests and the widget crate's can use it; a crate adapts it (a module that holds a
render at it, a helper that polls its own queue through `wait_until`) rather than writing a second
one, and the `test-waits` and `test-gates` rules below refuse one. It also holds the one
`Distribution`, a nearest-rank p50/p95 (`sorted[ceil(percent·n/100) − 1]`, always one of the
samples) that every timing figure is read from: the crates' own ignored timing tests print theirs
through it, and `xtask`'s timing tools write theirs through it. The `one-distribution` rule refuses
a second percentile or median definition.

The core holds its own work at a gate through crate-private, `cfg(test)` hooks. A test outside the
core that needs core work held reaches it through `luxforge-core`'s `test-holds` feature, which
only `[dev-dependencies]` turn on, like the disk-flush skip above (`test-holds-only-in-tests`
refuses it in any normal, build or workspace dependency). It has one hold so far:
`OwnerHandle::hold_listings`, which holds every index listing under a folder at each folder it
walks while the gate is shut; the desktop's long-work tests hold a listing there while they read
the board.

Every Cargo that `xtask` starts to build drops the package variables `cargo run` set for `xtask`
itself. `ring`'s build script reruns when `CARGO_MANIFEST_DIR` or `CARGO_PKG_NAME` changes, so a
build inheriting them would rebuild `ring`, `rustls`, `luxforge-core` and everything above them
after any build started from a shell, and the next shell build would rebuild them back. Without
them, builds from `cargo xtask`, `verify` and a shell share their artifacts.

### Verification tiers

`verify` runs a tier of the commands above and writes one summary. Each tier includes the ones below
it; the tiers above `quick` run the whole `check` in place of its quick subset:

| Tier | What it runs |
| --- | --- |
| `quick` | `check --quick`: every check and test but the [slow tests](#how-check-runs-the-tests) and the doctests. It builds nothing in release and launches no editor |
| `rendered` | the whole `check`, `editor-acceptance` and every checkout smoke scenario, including the macOS-only `visibility-monitoring`, `detail`, `detail-fit`, `detail-zoom`, `zoom`, `presets`, `export`, `settings`, `theme`, `gallery`, `controls`, `capabilities`, `performance`, `curve`, `lens-perspective`, `no-gpu-render`, `select`, `resolve-missing`, `develop-picks`, `filmstrip` (its RAW steps pending without `--source`), `loupe`, the three viewport scenarios and the six `mask-*` ones, through a bounded pool, then `gpu-qualification`, the release gate, alone, given `--manifest` when the run has one |
| `timing` | the whole `check`, `editor-acceptance`, then `editor-performance`, `editor-latency` and `measure`, in that order, serially, after everything else in the tier and behind the host-wide timing lock |
| `full` | rendered plus timing plus `hardening`, plus, with `--manifest FILE`, a `smoke --scenario raw-editor` run per manifest source (`raw-editor-<id>`), the owner-supplied authentic RAW tests via `raw-authentic`, a `smoke --scenario raw-panel` run and a `smoke --scenario raw-detail` run per manifest source and one `smoke --scenario performance` run over the first manifest source |

When to run each tier is in [when to verify](#when-to-verify). `hardening` needs only `--binary` and
runs in `full` whether or not a manifest is given. Without a manifest, `full` lists `raw-editor` and
`raw-authentic` as `skipped` with the reason `no --manifest`, and adds no per-source `raw-editor`,
`raw-panel`, `raw-detail` or RAW `performance` components at all, since there is no source to run them over.
`verify` reads the manifest through the one RAW manifest reader, so a manifest the `raw-editor`
scenario would refuse is refused before anything runs. `raw-authentic` runs the
`#[ignore]`d authentic-file tests in `luxforge-raw`'s `real_files` and the `raw::` module of
`luxforge-cli`'s `json_cli` (the ones that need only `LUXFORGE_RAW_OWNER_DIR`, not `real_files`'s
separate CC0-fixture test) with that variable pointed at the directory the manifest's own sources live in.

A skip is never a pass: a tier with any component `skipped`, `incomplete` (it ran and exited with the
incomplete code, as the release gate does without a source it lists or an adapter) or `not_run`, and
nothing failed outright, is `incomplete` rather than `passed`, naming which components and why in the headline and
in `summary.json`'s `incomplete` list, and its process exits with its own code (currently `3`),
distinct from `0` (passed) and the ordinary-failure exit code a real component failure uses.

Above `quick`, the command builds `luxforge-app` and `xtask` once in release, then runs each component as a child
process of the release `xtask` executable with its console output in `<out>/<component>/console.log`
and its own evidence in `<out>/<component>/run/`. `--binary PATH` is forwarded to every component
that takes one; without it the executable just built is passed explicitly, so every component
measures the same file. The rendered and timing tiers run `generate-fixtures` first when any
generated fixture — 24 MP, 60 MP, hue-wheel, presence, range or tone-ramp — is missing. A component that has
stopped making progress is killed after twenty minutes, the release gate after three hours, and recorded as `timed_out`. `quick` builds
nothing up front: its one component, `check --quick`, builds what it tests, runs as a child of the
`xtask` executable running `verify`, and its summary names no editor binary.

The rendered scenarios are the one block that overlaps: they run through a bounded pool, three at a
time by default and `--jobs N` otherwise, with `--jobs 1` as the serial run through the same path.
Each scenario is already a separate child process with its own output directory, catalog and evidence
directory; that every component's directory is its own is checked before anything starts. Scenarios
are started in the fixed list order and each result is stored at its own place in that list, so the
summary reads in list order however the completions interleave, and each one records the second of
the run it started at beside its elapsed time. `check` and `editor-acceptance` still run first and
serially, and the summary carries the pool's wall clock against the sum of the scenarios' own elapsed
times.

Timing components never overlap, with each other or with anything else on the machine. Before the
first one starts, `verify` takes a host-wide lock — `luxforge-timing.lock` in the OS temporary
directory, holding the pid, treated as stale only once that pid is no longer alive — and releases it
at the end of the run, including on failure. `measure`, `editor-latency` and `editor-performance`
take the same lock when run by hand, so an ad hoc timing run and a `verify` timing tier
refuse each other. A run that cannot have the lock refuses rather than measuring: it exits non-zero
naming the live pid, and its summary records `refused: another timing run (pid N) is alive` with
nothing run.

A figure is only as good as the host was. `verify` reads the one-minute load average immediately
before each timing component and records it beside that component and its timing rows, along with the
threshold of 8.0 — the baselines in the [performance plan](../specs/performance.md) were taken
between 2.3 and 5.7 on this fourteen-core host. Every timing summary states the threshold and the
load, on either side of it. When a component started above the threshold, all of its timing rows and
target verdicts are marked `unreliable` instead of `pass` or `miss`: the measured figure, its sample
count and the load are all still recorded, but the target is unanswered rather than met or missed,
and the summary header names the component. Load never fails the run by itself. Every tool that
records a load-dependent figure of its own (`editor-latency --mode paint`, `mask-range`'s stroke
latency, and `verify`'s rows and verdicts) records it through one rule and one shape,
`{"load_average_1m","load_threshold","reliability"}`, with `reliability` either `reliable` or
`unreliable`; a host that cannot report its load records a null load, marked `reliable`.

Every timing tool writes its figures in one shape, a `rows` array of
`{"metric","unit","distribution"}` in its own report: `editor-performance`'s `result.json`,
`editor-latency`'s `latency.json` in every mode (its GPU counters included) and
`resources.json`, `measure`'s `measurements.json` (`<workload>.<figure>` and `idle.<figure>`),
and `mask-range`'s `stroke_latency`. A distribution is
`{"count","p50","p95","min","max","samples"}` over the one nearest-rank `Distribution`; a single
observation (a one-shot core step, an idle window, frames per second, a GPU counter) is a
one-sample distribution, and a metric the run never reached has a `null` distribution.

The console shows only the Markdown table. `<out>/summary.json` and `<out>/summary.md` hold, per
component in plan order, its status, start offset, elapsed time, exit code, first failure line,
artifact paths and how many editor processes it started, then the p50/p95 timing rows of the timing
tier — every row of every timing component's report, read by one loop — with their source file,
sample count, load and reliability, and each provisional performance target, read from its row, with
its measured figure and a `pass`, `miss`, `unreliable` or `not_measured` verdict. Both
files are rewritten after every component, from whichever worker finished it, so a partial run still
reports what it has; components that never ran are `not_run`. The process exits non-zero and names
the components either way: an ordinary failure when any component failed or timed out, and the
`incomplete` exit code (above) when nothing failed but some component was `skipped` or `not_run`.

The command never opens a frame. Read a capture as an image only for a failed scenario or a design
review.

Wall-clock on the owner's M4 Pro, release build already current and the Cargo cache warm, on a host
shared with other work at one-minute load averages between 4 and 13: `quick`, with nothing to
rebuild, 3.9 to 6.5 s at load averages of 5 to 19, of which the test binaries are 2.3 to 3.5 s; before
the tests [skipped the disk flush](#tests-skip-the-disk-flush) it was 6.8 to 7.3 s at load averages
of 1 to 2. The whole `check` took 33 s at a load average around 35. `rendered` a measured 27-scenario workload with 36 editor launches taking 28 s of wall clock
through the pool against 82 s of their own summed elapsed time, the four `mask-*` scenarios the
longest of them at 4 to 12 s each; `timing` 70 s with the default sample counts, of which `measure` is
48 s and 17 launches, `editor-performance` 4 s and `editor-latency` 5 s; `full` with the owner's three-source
manifest adds one `raw-editor` smoke run per manifest source, two launches each, plus one `raw-panel`
smoke run per manifest source and one RAW `performance` smoke run over the first source. On a run whose
`measure` started at a load average of 8.33, that component's rows and target verdicts came back
`unreliable` while the two components below the threshold still gave verdicts.

### Rendered scenario cost: why every scenario stays in `rendered`

Per-scenario elapsed time comes from each run's own `summary.json`, and `verify --tier rendered --output NEW_DIR` (`--jobs 1` for serial figures) reproduces it. A scenario is one editor launch and costs one to two seconds, so against a rendered tier that stays under two minutes no single one is a material share of it. Every scenario that a checkout can open therefore stays in `rendered`; `full` adds only the RAW components (with `--manifest`, a `raw-editor` and a `raw-panel` run per manifest source and one RAW `performance` run). Two scenarios cost more because they wait in real time, and stay because nothing else in `rendered` checks what they do: `zoom` (two launches with three one-second idle waits each) is the only rendered check of the percentage zooms, and `performance` (the sampler needs real seconds to fill its window and to prove itself asleep) is the only rendered check of the Performance section and of its sampler's gating. The summary records the load average for timing components only. The Performance scenario has a 75-second process ceiling around the editor's 60-second script deadline, allowing its full 60 MP render and fixed sampler windows to report a result. Its checks validate sampled state and completion; latency measurements use the timing tier's quiet-host conditions. External memory comparisons use observation intervals wholly before and after each app sample, excluding calls that overlap it; the complete intervals must stay within 1,000 ms, with the existing 8 MiB memory slack.

## Extended camera corpus

This optional path needs Python 3.10+, AWS CLI v2 (CI pins 2.35.16), the Rust
toolchain above and private R2 access. It reads the checked-in
[manifest](../../fixtures/sample-corpus.json); it does not use the upstream site
during tests. The [design](../design/sample-corpus.md) records device coverage,
qualification limits, resource bounds and credential expiry dates.

Copy `.env.example` to ignored `.env`, set the account/bucket and S3 keypair,
and run `chmod 600 .env`. Read-only keys in `R2_READ_ACCESS_KEY_ID` and
`R2_READ_SECRET_ACCESS_KEY` are preferred for sync/test; only publication uses
the separate writer. Process `R2_*` variables override the file. The parser
reads literal values and never evaluates shell expressions.

```sh
# Inventory and a selected regression; downloads are cleaned after each chunk.
python3 tools/sample_corpus.py inventory
python3 tools/sample_corpus.py test --group canon --cleanup --output artifacts/corpus-canon

# Keep a selected local cache, then run without network access.
python3 tools/sample_corpus.py sync --group nikon --qualified-only
python3 tools/sample_corpus.py test --group nikon --qualified-only --offline --output artifacts/corpus-nikon

# Remove only selected, verified files from the tool-owned cache.
python3 tools/sample_corpus.py clean --group nikon --qualified-only

# Full extended run, bounded transfers and serial processing within chunks.
python3 tools/sample_corpus.py test --chunk-size 8 --chunk-mib 1024 --transfer-jobs 2 --cleanup --output artifacts/corpus-full
```

`--group` can repeat: `canon`, `nikon`, `sony`, `fujifilm`, `panasonic`,
`olympus`, `omsystem`, `pentax`, `leica`, `ricoh`, `dji`, `samsung` or `phones`.
`--shard 0/4` selects a deterministic quarter of source hashes (indices 0–3).
`--qualified-only` includes both full qualified and older mosaic-only references;
`--strict-development` additionally requires the M4 reference development
hashes. The full 341-file strict M4 repeat passes in about 11 minutes.
`mosaic_sha256` covers little-endian u16 source samples (interleaved for RGB);
monochrome second developments repeat unity gains after verifying that changed
white balance is refused. Linux uses portable metadata, integer and numeric checks. A candidate
decode or refusal remains unqualified, even when the command succeeds. To
require the entire target list to have samples and every selected test file to
have a frozen expectation, use `--require-complete`; the current acquisition
gaps make it fail even for a qualified-only test selection.

The default cache is `private/sample-corpus/cache`; choose `--cache DIRECTORY`
to isolate concurrent runs. A marker identifies tool-owned caches; foreign
nonempty directories and symlinks are refused. `--cleanup` removes only files
created by that invocation, preserving any already cached files. `clean`
removes selected cache entries only after verifying all their hashes. It never
deletes originals outside the cache or reports. A new output directory is
required for each test; it holds the exact input list, qualifier results, logs,
manifest hash, elapsed time and qualification/failure classifications.

Tests automatically build the release `qualify_profiles` example unless
`--qualifier PATH` supplies an existing binary. Ordinary repository checks do
not download samples. Offline controller tests are:

```sh
python3 -m unittest discover -s tools -p 'test_sample_corpus.py' -v
```

For maintainers adding licensed samples, review
`fixtures/sample-corpus-targets.json`, fetch the raw.pixls.us JSON index into an
ignored directory and rebuild the provenance manifest. Preserve reviewed
expectations with `--previous`; newly discovered files remain candidates.

```sh
python3 tools/build_sample_manifest.py --index private/sample-corpus/raw-pixls-index.json --previous fixtures/sample-corpus.json
python3 tools/sample_corpus.py publish --transfer-jobs 4 --cleanup
python3 tools/sample_corpus.py publish-index --transfer-jobs 4
```

The builder bounds HEAD requests and pins exact sizes. `mirror` downloads from
the pinned upstream URLs without R2; `publish` downloads/reuses verified cache
files and uploads immutable R2 objects, optionally reusing hash-matched
originals from repeated `--source-dir DIRECTORY`. Existing remote objects must
match size and SHA metadata and are never overwritten. `publish-index` verifies
the full manifest's remote object metadata and archives the immutable manifest;
`verify-remote` checks selected object metadata. Actual `sync`/`test` reads also
verify every object's bytes against SHA-256.

The [weekly/manual workflow](../../.github/workflows/sample-corpus.yml) uses only
GitHub's encrypted reader secrets and `R2_ACCOUNT_ID` repository variable. Its
Monday 04:23 UTC schedule activates on the default branch, runs four shards
with at most two jobs concurrently, limits each job to 30 minutes and retains
reports for 14 days. It receives no publishing credential and has no
pull-request trigger. Hosted Linux elapsed time remains unmeasured until its
first run; the strict 132-reference M4 adapter run takes about five minutes.

## Running the application

`cargo xtask develop` starts the editor. It owns the catalog (`--catalog FILE`, defaulting to the platform configuration directory), offers native Open with Cmd+O or Ctrl+O and starts an authenticated loopback JSON service. `--data-root DIR` isolates config, data and log paths. Every launch writes a bounded diagnostics log, `events.jsonl` (at most 4096 events, then one truncation record), to the platform log directory — `~/Library/Logs/Luxforge` on macOS, `DIR/logs` under `--data-root` — keeping the launch before it as `events.previous.jsonl`; an evidence run writes its log into its evidence directory instead. The application also accepts `--window-size W H` (320 to 4096 logical), `--developer`, `--disable-module MODULE_ID`, `--proof-endpoint URL` (registers the developer capability proof against that endpoint; refused without developer mode), `--hidden-window`, `--evidence-dir NEW_DIR`, `--evidence-script FILE`, `--evidence-gpu-identity`, `--no-gpu-render` and `--software-adapter`, and `--gpu-adapters` on its own. `--hidden-window` creates the window invisible: it owns a real surface and renders and captures through it exactly as a visible window does, but the window server never places it on screen. `--evidence-gpu-identity` is a test hook for the GPU preview stage, refused without `--evidence-dir`: the run draws its photograph at Fit through the photo surface's GPU stage with the identity program, over a boundary held off the UI thread from the frame on screen, which stays the fallback, and a capture waits for that draw ([GPU previews](../design/gpu-preview.md#where-the-code-lives)). Every automated editor launch the harness makes passes it; `develop` in either mode never does. `--no-gpu-render` refuses the photo surface's GPU stage for the launch, before the window opens, as a machine whose graphics adapter cannot run the stage does: the stage's capability check answers unavailable, the desktop hands it no plan and asks for no boundary, every frame is drawn on the CPU — a drag's ticks by the CPU proxy, the picture at rest by the reference renderer ([the CPU proxy](../design/instant-preview.md#the-cpu-proxy-a-session-without-a-gpu)) — each captured frame records the drawing path `cpu` with `plan_fallback` `no-adapter` and `state.surface.gpu.stage` `{"state": "no-adapter", "refused": true}`, the status bar says `Reference renderer` beside its render slot, at rest as well as during a gesture, and every client's `session.state` reports `renderer` as `{"record": "reference", "reason": "no-adapter"}` ([GPU-first](../design/gpu-first.md#the-contract)). It is read once at launch, in any mode, and is neither a preference nor a session field; the window itself is still drawn by the adapter, and the `no-gpu-render` scenario runs it. On a host whose only adapter is a software one (lavapipe, WARP) the GPU stage is refused the same way, since the software adapter is not adopted; `--software-adapter` draws through it for that launch, and the session and the status bar name it. `--gpu-adapters` prints the graphics adapters wgpu offers the renderer, among the backends `WGPU_BACKEND` names or all of them, one JSON object a line with each one's backend, name, device type (`Cpu` for a software rasterizer such as Mesa's lavapipe), vendor, device and driver, and exits before any window, catalog or log opens; `cargo xtask doctor` lists them through it once a release editor is built. `--developer` forces developer mode on for the launch, whatever the Developer mode flag in Settings says (on by default in debug builds): it lists proof and diagnostic modules, which are hidden by default so the workspace stays a photo editor, and the proof flags. The desktop reads its launch flags once, from the run's preferences, before it assembles the registry; an evidence run keeps its preferences inside its evidence directory, so no automated launch reads the person's flags ([settings and flags](../design/settings-and-flags.md)). `--disable-module MODULE_ID` registers that built-in as unavailable, keeping its effect identities readable so a stack that uses it reports the unavailable effect instead of rendering without it; an unknown identity is a startup error. Evidence mode is the same editor driven by the harness: each `--open` goes through the ordinary import call into a catalog created inside the new evidence directory, a window frame is captured after each outcome, the script's steps then run with a frame each, and the run exits after writing its results. Manual Open is disabled during collection, and `--open` may repeat only with `--evidence-dir`.

The headless owner, `luxforge-json`, reads one JSON request per line. It is the `luxforge-cli` crate's binary, which builds without the GUI stack: `cargo xtask build [--release]` builds it with the editor, and `cargo build --release --locked -p luxforge-cli` builds it alone.

```sh
target/release/luxforge-json --catalog /path/to/catalog.sqlite < requests.jsonl
```

Start with `schema.list`. Request shapes and live-session behavior are in the [user guide](../user-guide.md). `--data-root DIR` puts module settings, grants and installed resources under that root instead of the platform directories, `--secret-store memory` keeps module secrets for the process only instead of the platform store, `--permission-authority` lets this client grant module consent (an explicit local setup step; without it `module.permission.grant` is `forbidden`), `--developer` serves the test modules, the pixel and controls proofs, as the desktop's developer mode does, though `luxforge-json` reads no flag and is never in developer mode without it, and `--proof-endpoint URL`, which needs `--developer`, registers the capability proof module. The desktop's own client always has that authority; an evidence run keeps module state inside its evidence directory with an in-memory secret store, so no automated run touches the person's configuration or login keychain. Only one process owns a catalog at a time; a second instance exits with an explanatory error. Diagnostics go to stderr, or to isolated logs under an explicit data root, never to protocol stdout. Editor mode writes only the catalog and a temporary live-session file beside it; the source original is never written.

On macOS, `develop --background` builds the selected profile and runs a temporary copy in an `LSBackgroundOnly` app bundle, preventing desktop activation. Use an isolated catalog or `--evidence-dir NEW_DIR` for automated checks. The live API and native GPU renderer remain available; this mode is for API and capture work, not keyboard, mouse or native-dialog checks. The bundle is removed after exit, and the original executable and packaged app are untouched. Restricted tool environments must permit macOS LaunchServices/window-server IPC: a background process can otherwise stall before image work, with only startup/open-request events and idle source/catalog workers. Retry with the required host access rather than activating the window. Ordinary `develop` remains an interactive launch. `--background` fails explicitly on other platforms.

### Browsing the component gallery

Debug builds expose the title-bar **Developer** button automatically. To inspect the gallery in
an optimized build, run `cargo xtask develop --developer` (automated launches add `--background`).
Its page chooser and Previous/Next controls browse nineteen pages; Back to editor or Escape returns.
The `gallery` smoke covers all 129 named widget states and the return to the unchanged editor via
the same view message as the button; the page is desktop view state, so the session's workspace
stays unchanged throughout.

## Rendered evidence

Smoke runs the built or packaged editor through a deterministic evidence sequence (repeated `--open`, evidence directory, fixed window size, bounded deadlines); there is no separate viewer, so the captured frame is the editor window with its sidebar. Every scenario is one row of the runner's table, which `cargo xtask smoke --list` prints with its launches and frame counts, what it opens and its window; generate the large fixtures first. Each launch has a plan: every frame it captures, in order, with the script step that produces it and what that frame must show. The script the launch runs and the number of frames it must capture are derived from the plan, and every frame of every scenario is checked against it before the scenario's own checks: its run identity, backend, renderer-readback provenance and state file; for a scripted frame, the step the editor recorded equal to the scripted one as its parser reads it back, listed in the run's script and log and `sent` unless the plan expects it refused; an input error exactly when the plan expects a refusal; then the step's own expectations, such as the commit it makes, the history label, a layer's payload or identity, a field's text, the draft, the sections expanded, what the Masks panel shows (the masks, the open mask, its components' names, modes and kinds, the selected row, the overlay control, its menu and the Brush section), the notices, the status line, workspace fields such as the canvas mode and the overlays, and the zoom. Each launch's `plan-checks.json` records what every step was checked against, and a failure names the step. What a plan cannot say, mostly what the pixels show, a scenario checks itself, and records through one recorder, `scenario::Checks`: each pixel claim is a comparison of two readings under a stated tolerance, recorded with both readings whether it holds or not, beside notes of what a frame shows, all written once as `<scenario>-checks.json`. Ignored `xtask` tests check a change to the plans or to how scripts are written: `SCRIPT_DUMP=DIR cargo test -p xtask dump_script -- --ignored` writes every launch's script and argument list as a launch writes them, every script and argument list `editor-latency` can write (its `--zoom` and `--moving-pan` included), and the argument list of every `measure` and `hardening` launch, to compare against the same dump from before the change (a row whose plans read more than its sources, such as `raw-editor`'s neutral point, is dumped over the placeholder its table plan names), and `SMOKE_RECORDED=DIR cargo test -p xtask mutations -- --ignored` (`DIR` absolute, since the test runs in `xtask/`; it fails when no run is found) replays each `DIR/smoke-<scenario>/run`, a supplied source given again as the one its first launch opened, with its plan's last step removed and with its first expected label changed, and fails unless each replay fails on the frame count and on the named step. Each run writes `result.json`, `app/events.jsonl`, `app/state.json`, `app/frame-*.png` (window-renderer readbacks, not OS screenshots), `subprocess.log` and `reproduce.md`; a scenario of several launches writes each one's evidence to its own directory and log instead, and `result.json` lists every editor process a run started under `launches`, with its command, its exit code and the adapter its last frame recorded (`adapter`), which is what `verify` counts. An editor's diagnostics log holds at most 4,096 events: past that it counts what it drops, writes one `diagnostics_truncated` record naming the count when it finishes, and an evidence run whose log was truncated fails exactly as one whose log could not be written does. `smoke --verify-only RUN_DIR --output NEW_DIR` copies a recorded run, less the checks files its checks wrote, and runs the scenario's own code over the copy with each launch taken as the recorded one: the checks files it writes are that run's checks again, and `replay.json` is the verdict. The scenario comes from the run's `result.json` unless `--scenario` names it, and a `--source` run is given its source again. The `capabilities` scenario's endpoint record and secret scan are what the live run's own process saw, so a replay carries them from the recorded checks. Each frame records `surface_columns`, the physical x range of the photo surface derived from the editor's layout constants, `canvas_rect`, the canvas region between the panels and the bars as `[left, top, right, bottom]` physical pixels, which at a percentage zoom is exactly the scrollable the photograph pans in, `fit_rect`, the area Fit lays the photograph out in, and `photo_rect`, the rectangle the canvas draws the photograph into, right and bottom exclusive and snapped as the photo surface snaps it, carried past the capture's edges by a percentage zoom and `null` when no photograph is drawn. Scenarios locate the photograph by `photo_rect` alone: `Frame::photo` requires it inside the capture and holding a drawn picture, `Frame::visible_photo` clips it to the canvas, and where a claim is about placement `Frame::photo_edges` ties it to the capture, the pixel just inside the middle of each edge drawn and the one just outside the canvas surface; the runner verifies fixture colors, Fit geometry and centering within that range, generation and state, backend, exit status and unchanged source hashes before writing `passed`; blank, stale or missing frames fail. The `render_ready` event marks the upload of the open request's preview raster, which is when a frame becomes capturable. A script step's `script_step` record is followed, once what the step waits for has happened, by one `script_step_settled` record naming the wait (`waited_for`, such as `preview`, `slider_draft` or `session`) and the outcome that ended it (`by`, such as `presented_photo`, `presented_draft`, `draft_refused` or `session_answered`); a refused step records `script_step_failed`. The desktop's seams report those outcomes as typed values (`app/outcome.rs`) that only the evidence driver reads, and the desktop builds an event only when it has a log to write it to: an evidence directory, `--data-root`, or stderr when either was asked for and the log could not be opened. Single-open evidence has a 25-second application deadline; multi-step evidence scripts have a 300-second application deadline for repeated full-photo and RAW evaluations. Ordinary smoke retains its 35-second process deadline; the RAW editor journey has a 70-second process deadline and the Detail journeys have a 310-second process deadline. Detail functional quiet waits are 10 seconds. The macOS `visibility-monitoring` scenario has a 420-second deadline for its nine 30-second observations; capability notification qualification has a 160-second deadline for three held 20-second observations. Observation steps suspend evidence ticks and captures during the window. These are harness hang bounds, not interactive latency targets.

Each frame's `state.backend` names the adapter that drew the window as wgpu describes it: Iced's own name for it and its backend (`adapter`, `backend`), and the rest from an enumeration of that backend on the blocking pool, matched by backend and name: `device_type` (`Cpu` for a software rasterizer such as lavapipe, `IntegratedGpu` on the M4), `vendor`, `device`, `driver` and `driver_info`, each `null` when no enumerated adapter matched. The log records it once as the `backend` event, a capture waits for it, and a scenario run fails unless its last frame identified the adapter, which `result.json` records as `backend` and each launch as `launches[].adapter`. Each frame also records the session's renderer (`state.renderer`, `{record, reason}`, which every API client reads in `session.state`) and the photo surface's GPU stage (`state.surface.gpu.stage`, `{"state": "unchecked" | "available" | "no-adapter" | "device-lost", "refused": bool}`). The desktop reports the renderer to the owner whenever the stage's answer changes, logging `renderer_reported` with the renderer and the stage, or `renderer_report_failed` when the owner could not take it, and a capture waits while a report is on its way, so a frame's session names the renderer that drew it.

Each photograph handed to the display is logged once as `preview_displayed`, with its entry,
snapshot, source fingerprint, generation, draft revision and `dimensions`, the exact output stage's,
which is what picks, the percent-zoom box and the overlay cell grid map through. `path` is `gpu`
for a committed stack the GPU presents with no CPU render, whose `render_ms` is null, and `surface`
for a frame the CPU rendered: the reference renderer's exact frame, `reduced` when it was reduced to
the bounds of a view that draws it smaller, or, in a session without a GPU, a drag's CPU proxy
(`proxy`, with `proxy_approximate_reason`; [the CPU proxy](../design/instant-preview.md#the-cpu-proxy-a-session-without-a-gpu)).
`picture` says whose picture of the content is on screen (`gpu` or `reference`),
`approximate_white_balance` marks a drafted RAW white balance approximated on the developed planes,
and `reason` is `"zoom"` when the frame is a retained raster a zoom change needed rather than a
render. A CPU frame's `render_ms` is the preview worker's own time for the phase that produced
those pixels — the proxy build when that frame built it plus the render, or the exact render plus
the reduction — excluding the queue wait, source preparation and the hand-over, and for a `"zoom"`
frame the time recorded with that retained raster. It is the figure the status bar states as
`Approximate render · N ms` or `Exact render · N ms`; a frame the GPU draws is stated as
`GPU render · N ms` at rest or `GPU preview · N ms` in a gesture, with the interface thread's time
to draw it.

The photograph is drawn by a **photo surface** at every zoom: a shader primitive that owns its
wgpu texture, writes the raster it is given into that texture during the frame that draws it, and
recreates the texture only when the raster's dimensions change. At a percentage it is the whole
zoomed box inside the canvas's scrollable and hands the renderer only the part on screen, so the
render pass's viewport stays within the window whatever the zoom; wgpu refuses a viewport, or a
texture, larger than the device limit Iced requests, 8192 px a side. An exact render larger than
that — the 60 MP fixture is 10000 px wide — is held in a grid of textures, each carrying one extra
pixel on every side that has a neighbour, so the tiles meet without a seam. So `preview_displayed`
is emitted in the update that makes a raster the surface's source, once per raster, and carries
`"path": "surface"` to say so; the pixels are on screen in the redraw that update requests, with no
image-allocation round trip on the input path and no upload message to wait for. `state.json`
carries `surface: {view, raster, version, texture_writes, views}`: the session's zoom and pan, the
size of the raster on the surface, its version (the count of rasters handed over, so the version-th
`preview_displayed` is the one that put it there), how many rasters the surface has written into
its texture, and how many times the view has been built. A redraw of an unchanged raster writes
nothing, however often the view is rebuilt, and there is no upload step to time. The clipping overlay, the mask coverage and the
crop draft's input stage are drawn by the same surface from frames of their own, handed over in
the update that has them: `clipping_overlay` and `mask_overlay` are emitted in that update. For
ordinary photos with clipping enabled, capture waits for a GPU draw containing both the requested
photo identity and that request's assigned clipping frame version (`drawn_clipping_version`); if
the clipping version changes while a screenshot is in flight, capture retries. Empty, crop, gallery
and render-error captures do not wait for this pair. If clipping derivation or presentation fails,
the active script step is marked failed and the refusal frame is captured. Mask coverage still
settles a waiting step in its update, and the crop draft's open frame is drawn over its input
stage in the update that takes that stage up. `preview_exact_cancelled` records a reference job a newer request superseded, under its own generation and with `draft` when it was a crop draft's input stage, and
`clipping_overlay` carries `approximate` when the overlay was derived from a frame that approximates
a drafted RAW white balance. `preview_failed` records every failed preview of the displayed state,
with its entry and reason, and a scripted step waiting for the newest preview's pixels ends on it
and captures the failure. `preview_withdrawn` records a failed preview whose target was not the
picture on screen: that picture, named with the target, is taken off the surface with everything
derived from it, so `state.surface.raster` and `state.stack.displayed` are null until a frame of the
target renders, and `crop_draft_failed` records a draft whose input stage could not be rendered or was superseded before it rendered (`error_code: cancelled`, with that job's `generation` when it had one). A
crop draft's `state.crop.input_stage_loaded` says its input stage is on the surface. `state.json` carries `reference: {bounds, reduced, proxy,
proxy_approximate_reason}`, the bounds the view reduces the reference's frame to, whether the
texture on screen is smaller than its stage and whether it is a drag's CPU proxy, and `status_bar:
{message, render, fallback, gpu_ms, render_ms, render_approximate}`, what the bar drew and the
figure behind it. The `histogram`, `basic`, `large24` and `large60` scenarios check that every
`preview_displayed` of a CPU frame carries a finite `render_ms` below 5 s and that each captured
status bar states one of those figures in the editor's own wording, `Approximate render` exactly
when the frame on screen approximates a drafted RAW white balance, or a GPU frame's own figure.

### What editor-acceptance proves

`editor-acceptance` keeps only what `cargo test` cannot prove at the same layer: a chapter or step
exists only if no `cargo test` proves it at that layer. Five things remain, each driven through the
JSON method table with `OwnerHandle::call` as an independent client, against its own catalog inside
the run's output directory: the [field-patch conformance
suite](#the-field-patch-conformance-chapter) in release, [Basic's numerics on the photo fixture
against the independent reference](#the-basic-and-histogram-acceptance-chapter), the [placement of
Presence, the mixer and the vignette](#the-field-patch-conformance-chapter), the [Tone curve's
placement, masked order and sample query](#the-field-patch-conformance-chapter) (`tone_curve`), and
a [masked catalog reopened through a fresh owner](#the-masking-acceptance-chapter). Everything the
core's own tests prove stays there: history order, the read-only preview, undo, redo and restore,
the orientation layer, the crop and reopen are `editor::history`, `editor::plan`,
`modules::crop`'s tests, the module and method discovery is the [descriptor
snapshot](#the-built-in-descriptor-snapshot) and the method table's, and the mask commands, their
refusals, a disabled maskable module and a missing or changed original are the `mask` test binary's,
the command family's own tests and the conformance suite. A new step here needs a property that only
a release build, an independent oracle, a second process lifetime or a fresh owner can show.

### The Basic and histogram acceptance chapter

`editor-acceptance` starts with a chapter that drives Basic and the histogram's numerics through
the JSON method table with `OwnerHandle::call`, exactly as an independent client reaches it, against
its own catalog inside the run's output directory. Its oracle is the independent f64 reference under
`crates/luxforge-reference/src/`, compiled into `xtask` through a `#[path]` module rather
than copied, so the acceptance journey and the core's own numerical tests check production against
one written-from-the-formulas implementation that production code can never import; the crop
sampler, the exact quarter turn and the histogram reduction the chapter compares against are written
in `xtask/src/basic_acceptance.rs` from the crop spec and the histogram contract, not taken from
`analysis::reduce`. Every result lands in `result.json` under `basic_and_histogram`, and any
mismatch fails the command.

The chapter covers Basic's numerics on the photo fixture: `edit.set-basic`
checked whole-raster against the f64 reference for one field and for the frozen unit order of all of
them, `analysis.request/read` on current, historical and drafted targets against an independent
reduction, cropped-population semantics, and mixed stacks (a straightened 10° crop against a stepwise
quantize-then-bilinear reference, a point replacement before and after the Basic layer, and Basic
under an orientation layer). The host behaviour Basic shares
with every field-patch module is the
[field-patch conformance chapter](#the-field-patch-conformance-chapter)'s. Basic's ten fields in their
frozen order and the analysis methods' discovery are the descriptor snapshot's and
`luxforge_core::api::methods::tests::host_and_generated_methods_are_unique_complete_and_match_the_schema`'s.
The supersede and disconnect races, analysis sharing and one client's cancel of a shared job, and a
historical selection and its analysis staying attached through another client's commit are covered by
`luxforge_core::api::owner::tests::racing_requests_supersede_the_pending_job_and_withdrawal_releases_only_its_own_interest`,
`luxforge_core::api::owner::tests::two_clients_share_one_job_and_keep_independent_current_and_historical_results`
and `luxforge_core::api::owner::tests::historical_preview_stays_selected_during_another_clients_commit`
and are referenced rather than duplicated.

### The field-patch conformance chapter

Basic, the Tone curve (`luxforge.curve`), Presence, the colour mixer, the vignette and the developer
controls proof are one declarative field-patch module each, and the host behaviour they share is
proved once, for every module the built-in registry and the controls proof hold in that shape, by
one suite in `crates/luxforge-core/tests/modules/conformance/`. The suite finds the modules from
their descriptors — one effect, one `patch` action whose parameters are all fields with defaults (a
number, integer, boolean, enum, colour or curve), and the parameterless action the module reset
names — and derives every payload it sends from the declared field table, so a new field-patch
module is checked the day it is registered. It refuses to run when it no longer recognises one of
the five built-in ones or the controls proof, whose fields are the non-numeric kinds. The controls
proof's layer changes no pixel, so it is held to the in-process checks below and to compiling to
nothing and sharing the source allocation whatever it holds, not to the pixel consequences and the
journey through the method table. Every `patch: true` action of every registered module, `set-raw`
included, keeps the generic patch check's rules: an empty patch is filled with nothing, a declared
default alone is exactly that field, and a value its declaration refuses is refused by name. The
same function runs twice: as the core's `modules` integration test (`field_patch`) in the dev
profile, a [slow test](#how-check-runs-the-tests) the quick tier leaves out, and in release inside
`editor-acceptance`, which records what it returns under `field_patch_conformance` in `result.json`.
Each module runs against its own new catalog under the run's `field-patch-conformance` directory,
and a failure names the module, the step and the property that broke.

For each module the suite checks, in process: every neutral spelling of the payload (`{}`, every
field at its default, each field alone at its default and in another spelling of it, zero number
defaults written as `-0`) compiles to no units, is reported neutral and `Neutral`, renders the
source's own allocation and changes no byte on the linear path; each field moved alone and each
whole payload has exactly the consequences of the module's own neutrality rule; every field reads at
the edges of its declaration (both ends of a range and a whole maximum as a JSON integer, both
booleans, every option, black and white, a moved curve), while a value its declaration refuses (just
outside a range, the wrong JSON type, an undeclared option, a channel past 255, too many curve
points), a non-object payload, another effect's payload and an undeclared format are refused by name
without being rewritten; a patch plans a commit only for a non-neutral layer holding the canonical
payload, merges over the stored layer and updates it in place, is a no-op when it changes nothing
however it is spelled, drops a field set back to its default, and a reset keeps the layer and stores
`{}`; each field of every kind alone commits, is a no-op set again in any spelling, updates in place
and clears to `{}` at its default; two layers for one target refuse planning,
rendering and sampling by name; history labels follow the declared rules (one field by its value, a
group's reset preset as `Reset <group>`, the module reset, a field count, none for an empty patch);
and a layer reports every field with its defaults filled and describes its moved fields in declared
order. Then, through the JSON method
table as independent clients: discovery (`module.list` serves the registry's descriptor, and
`schema.list` lists the patch's fields as optional in declared order, the reset with none, the mask
target exactly when the effect is maskable, and every host method the journey uses); a neutral first
set and a reset without a layer committing nothing; request refusals; `draft.begin`, `draft.set` and
`draft.cancel` leaving no entry, revision, event or open draft; a gesture committing exactly one
entry, labelled by the declared rule, storing the patch as sent and the canonical payload, and
rendering what its draft previewed; return-to-start and repeated values as no-ops; a retried request
deduplicated with no event and the same request id with other input a conflict; a whole patch
updating the one layer in place; for a maskable effect, one layer per mask target beside the global
one and two layers for one mask refused; group and module resets keeping the layer's identity; undo,
redo, a read-only preview and restore each returning the stack of the entry they name; `render.sample`
equal to the rendered raster, and `sample` equal to `render` on the byte path and on the RAW linear
path over the same pixels, at the corners, edge midpoints, centre and a 4 × 3 stride, through the
global and masked layers, an axis-aligned crop and a straightened, resampling crop; two clients (a
conflicted draft, a refused commit, a reapply keeping both fields, a historical selection surviving
another client's commit); the same catalog served with the module unavailable keeping every layer
readable and refusing sampling, analysis and a new edit by name; and a reopen returning the same
revision, entry, layer and mask identities, rows, pixels and analysis identity. The original's bytes
are unchanged throughout.

A module's descriptor is the [built-in descriptor snapshot](#the-built-in-descriptor-snapshot)'s.
The words its history labels use for each field, its numerics against its frozen reference and
its unique behaviour — Basic's neutral picker,
Presence's halos and tiling, the vignette's recentring against its frozen reference — stay in that
module's own tests under `crates/luxforge-core/tests/` and `src/modules/`, and Basic's
numerics on the photo fixture in the Basic and histogram chapter. The placement of Presence, the
mixer and the vignette is `editor-acceptance`'s Presence, mixer and vignette chapter
(`xtask/src/presence_mixer_vignette_acceptance.rs`, under `presence_mixer_vignette` in
`result.json`), driven the same way: Presence after the colour run and before the geometry tail in
every touch order, the mixer after Basic in both touch orders with the same bytes, and the vignette
last and recentred on the stage each crop update produces. The Tone curve's own behaviour is
`editor-acceptance`'s Tone curve chapter (`xtask/src/curve_acceptance.rs`, under `tone_curve` in
`result.json`, with its time as `tone_curve_chapter` under `timings_ms`), driven the same way: the
curve after Basic and before the mixer in every touch order with an identical rendered raster;
masked curve layers on two masks after the global one in mask-list order and before the mixer, and
re-sorted with the masks by `mask.reorder {mask, index}`; `query.sample-curve` for the displayed
entry's stored points equal to `luxforge_reference::curve::curve` at all 257 samples to `1e-12`;
and a generated 8-bit grey ramp of every code (whole flat JPEG blocks, so each decodes to exactly
its code) through an S-curve layer within one code of the quantized reference at every code, with
the number of off-by-one codes recorded. The frozen fixture through render on both paths is the
core's `modules` test (`curve`).

### The built-in descriptor snapshot

What the built-in registry publishes is written down once, in
`fixtures/modules/builtin-descriptors.json`: `module.list` with its `host` array, and every method
`schema.list` generates from the modules' and the host's descriptors, with its parameters, source
kinds and superseded fields. `crates/luxforge-core/tests/modules/descriptors.rs` serves
`ModuleRegistry::builtin()`, an ordinary run's registry with no test module, through an owner,
lists both as a JSON client and compares the result with the file byte for byte, so which modules, effects, actions, queries, controls and variants the
build declares is this file's and no other test keeps a list of them; a change meant to keep the
descriptors identical proves it by passing. After an intended descriptor change, regenerate it and
review the diff:

```sh
cargo test -p luxforge-core --test modules -- --ignored generate_builtin_descriptor_snapshot
```

Beside it, `schema_list_names_developer_only_methods_only_in_developer_mode` serves
`ModuleRegistry::developer()` too and checks that it lists every ordinary method unchanged and adds
exactly the methods the test modules generate, none of which the snapshot holds.

### The masking acceptance chapter

`editor-acceptance` also ends with a masking chapter, in `xtask/src/mask_acceptance.rs`, driven the
same way: every step is one JSON request through `OwnerHandle::call`, against its own catalog inside
the run's output directory, with no desktop, no window and no pointer. It proves one thing no
`cargo test` proves at that layer: a catalog written by one owner and read by a fresh one returns
what the first wrote. The five `mask-*` smoke scenarios are where a gesture's own frames and
correlated state live; the mask commands, their refusals, an agent committing under an open mask
gesture, history across mask entries, a maskable module served unavailable and a missing or changed
original are the core's own tests (`tests/mask/commands.rs`, `mask/commands/plan_tests.rs`,
`editor/masks.rs`, `tests/modules/conformance/` and `editor/history.rs`), and the mask kinds'
pixels are the kind-conformance suite's.

The first owner builds the catalog: all five component kinds created from JSON (a gradient, a
radial, the two range selections and a brush through `mask.add-stroke`), one mask composing three
kinds in the three modes, a second stroke on a brush, an amount, an inversion, a geometry patch, a
rename, a masked Basic layer, a masked Presence layer, a masked mixer layer and a masked Tone curve
layer beside the mixer on the radial mask, and two duplicates that copy the bound layers. On the way
it proves the masked curve's placement: `mask.list` lists it once, before the mixer; it changes the
pixels the inverted radial covers and not the core it excludes; the radial mask's duplicate holds
the same layers in the same order, its curve layer placed after its source's; and `mask.reorder`
moving the copy first re-sorts the masked curve layers in `asset.state` into the new mask order.
`render.sample` equals the rendered byte at every probe after the curve and after the reorder. It
then stops. A fresh owner over the same file returns the same revision
and current entry, the masks, components, payloads and bound layers by identity, and the same
`render.sample` values at six positions across the frame. Everything lands in `result.json` under
`masks`, the masked curve's figures under `masks.masked_curve`, and any mismatch fails the command.

One contract shapes how it reads pixels: it reads them with `render.sample` and never renders a recipe
fetched over JSON. A brush component's payload holds its strokes by content address and the resolved
strokes are never serialized, so such a recipe has addresses and no points and rendering it outside
the catalog that holds the store is refused by name. That is the retention contract working. The
oracle `render.sample` is compared with is therefore the owner's own bound stack for the current
entry — its preview evaluation, with every stroke's points bound in from the store — rendered in
process on the fixture's decoded source with the owner's registry.

### Authentic RAW evidence

The `raw-editor` smoke scenario (`cargo xtask smoke --scenario raw-editor --source RAW --manifest FILE --output NEW_DIR`) runs the actual background editor over one RAW file, then reopens the first launch's catalog in a second process. The edit launch passes exposure, gain and custom temperature/tint edits, a sensor-neutral pick at the manifest's point, geometry, undo, Original/current history selection twice (the second pair is the hold-`\` compare once both developments exist) and 100%/Fit. Every frame of both launches passes the plan's universal checks; the scenario's own check the imported source against its manifest entry, a ready render of the displayed entry on every frame with Basic's Exposure, Temperature and Tint showing the recipe they mirror, the displayed entry and layers against history, that each edit moves the photograph's central window and that the historical Original and the reopen show the same picture as the frames they repeat, and the source hash. These comparisons prove reevaluation and state correlation, not controlled color accuracy. Each launch has a 70-second process deadline, since large RAWs and required DNG corrections redevelop the mosaic on several steps. `raw-editor-checks.json` records the comparisons and, as one functional run's observations rather than a distribution, each step's request-to-display time; RAW memory under a heavy edit is the `performance` scenario's over a RAW `--source`. `verify --tier full --manifest FILE` runs the scenario once per manifest source; repeated trials are repeated runs.

A RAW manifest has `format:1` and a `sources` array, and every tool reads one through the one reader, `raw::manifest`, which refuses unknown fields, a file over 1 MiB, an empty or over-long list, a duplicate id or one that is not ASCII letters, digits and hyphens, and a SHA-256 that is not lowercase hex. The editor manifest `verify` and `raw-editor` take gives each source `id`, `path` (relative to the manifest unless absolute), `sha256`, the editor's `mode`, `make`, `model`, upright `source_dimensions:[width,height]`, `orientation` and a fixture-verified `neutral_point:[x,y]`; a mode the [camera catalog](../../crates/luxforge-raw/data/cameras.json) does not declare is refused as the manifest is read. A source whose camera profile declares DNG corrections additionally supplies exact `sensor_dimensions`, `active_area` and `default_crop`, consistent with each other and with the upright size; the scenario checks the import recorded them and a correction provenance for each applied opcode, while the opcode order, calibration and interpretation the profile requires are the RAW adapter's own checks on import. `raw-editor` finds a source's entry by its SHA-256 and keeps a copy of the manifest as `manifest.json`, which `smoke --verify-only` reads again. Keep private paths and derived evidence ignored. Use an explicit absolute `--binary` and the same `CARGO_TARGET_DIR` for build and harness when working across worktrees.

Native and JSON authentic-file tests are opt-in, ignored in the normal test suite:

```sh
LUXFORGE_RAW_OWNER_DIR=/path/to/private/raw LUXFORGE_RAW_PUBLIC_DIR=/path/to/cc0/raw LUXFORGE_RAW_POPULAR_DIR=/path/to/popular/raw cargo test --release --locked -p luxforge-raw --test real_files -- --ignored --nocapture
LUXFORGE_RAW_OWNER_DIR=/path/to/private/raw cargo test --release --locked -p luxforge-cli --test json_cli -- --ignored --nocapture raw::
```

The absence of private fixtures is a skip, not passing authentic-file evidence. Synthetic/reference tests remain normal CI checks.

### Evidence scripts

`{"pinch":{"delta":0.1823215567939546,"x":0.37,"y":0.42}}` supplies a synthetic native magnification increment through the trackpad handler. `delta` is finite and logarithmic (`ln(1.2)` zooms in by 20%); `x` and `y` are canvas fractions in 0–1. It waits for the resulting view's pixels. The `zoom` scenario checks off-centre pinch in and out on both generated photo sizes, source-point anchoring, unchanged history and source hashes, and on macOS installation of the native event monitor. This proves the input handler and rendered result, not physical trackpad delivery or feel.

`--evidence-script FILE` takes a JSON array of steps. They run in order after the last `--open`
outcome, each ends in exactly one captured frame numbered after the open frames, and each frame gets
its own `state-<n>.json` and a record in `result.json`'s `script`. A capture reads back the frame
the window renderer drew last rather than drawing a fresh one, so it is taken only once that frame
was built after every update the editor has handled, and the frame's state is recorded at that same
moment: a preview result that arrives beside the capture tick waits for the next frame instead of
leaving the state describing a picture the capture does not show. A step whose frame is due when a
view change asks for a new frame — a zoom that needs a picture at new bounds, a panel toggle — waits
for that frame, so a capture never shows a picture made for the previous bounds. Every step goes through the
messages and owner calls the controls use, so a script exercises the real paths rather than a
parallel implementation. Parsing happens before the window opens; at most 64 steps.

The steps are one set of serde types, the `luxforge-evidence` crate: the desktop parses a script
with them and xtask builds every scenario's script from them, so a step has one spelling on both
ends. Parsing is strict. An unknown step kind or field, a missing field, or a value of the wrong type
or out of its range fails the whole script with an error that names the step's position and kind,
such as ``evidence script step 2 (slider): missing field `action` ``, and a refusal never quotes a
secret. A script xtask writes is in its canonical form, which is also the form the editor records
each step in: defaults a hand-written script may leave out, such as a slider's `release` and
`cancel`, are spelled out, and a step carrying a secret is kept and recorded with it redacted.

```json
[
  {"api": {"method": "edit.crop-fit", "params": {"aspect": "16:9", "angle": 0}}},
  {"draft": {"start": true}},
  {"draft": {"rect": [40, 24, 300, 200]}},
  {"draft": {"preset": "1:1"}},
  {"view": {"zoom": 100}},
  {"draft": {"apply": true}}
]
```

Each step is an object with exactly one key.

- `api` sends one owner request. For a method that is not an edit of the open asset, such as
  `preset.list`, `session.state` or `preset.delete`, the request is sent as written, with the open
  asset's `asset_id` only when the method names one and a fresh `{request_id, actor}` envelope when
  `schema.list` says it carries one, and its frame is captured when it answers; the answer is
  recorded as the step's `result`, and after a `preset.*` method the library is listed again before
  the frame is captured. For an edit of the open asset — a method whose schema names the
  `revision` envelope and an `asset_id` — the desktop fills `asset_id` and the `mutation` envelope
  itself — the current state's revision and a fresh request id — and rejects a script that sets
  either, so any asset mutation works, `history.undo` included. Its frame is captured when the resulting preview
  reaches the GPU: the same `render_ready` correlation an `--open` uses. A module settings write
  (`module.settings.*`, `module.profile.*`, whose schema names the `revision` envelope and no asset)
  carries the settings revision the desktop holds for its `module_id`, and a `profile_id` may name a
  profile by `{"name": label}` or by its position in the module's settings read, resolved to the
  identity the host assigned. After any `module.*` or `task.*` method that names a module, the
  desktop reads that module's settings and status again, as it does after another client's change,
  and the frame is captured once that read has answered and any job it found reports progress. A
  `module.settings.set-secret` step's `value` is kept and recorded as `<redacted>`.
- `agent` sends one edit of the open photograph through a second client, an agent editing beside
  the person: `{"agent": {"method": "edit.transform", "params": {"transform": "rotate-right"}}}`.
  The client is registered on the desktop's own owner at the run's first `agent` step and
  disconnected when the run ends. The desktop fills `asset_id` and the `mutation` envelope as an
  `api` edit's, with the revision it holds, a fresh request id and the actor `evidence-agent`, resolves name
  references the same way, and rejects a script that sets either. The desktop sends nothing itself: the owner wakes its event sync
  for the other client's change, the sync reads it back as a change made elsewhere, and the frame
  is captured once the agent has its answer and the frame of the entry that request committed has
  been presented (`waited_for` `agent`), so an open draft is conflicted exactly as another client's
  commit conflicts it. An answer that commits nothing is captured on the next frame, and a refused
  one is recorded as failed. The event sync runs in every evidence run as it does in a session; it
  reads only what the owner wakes it for, which is another client's change or a job of the
  desktop's own that ended (an import, an export, a module task). An `agent` step may also send a
  host method, such as `{"agent": {"method": "preferences.set", "params": {"theme":
  "luxforge.dark"}}}`: as written, with `asset_id` when the method names one and a request
  envelope with the agent's actor when its schema names one (a module settings write is refused).
  It is captured once the agent has its answer, the event sync has read past the event that answer
  was given at, and nothing the sync started reading — the preferences, the theme library, the
  flags, a theme to draw — is still in flight (`waited_for` `agent_host`).
- `draft` drives the crop draft: `start`, `reapply`, `angle` (typed into the angle's box and
  submitted), `nudge` (`-1` or `1`: one press of the angle's − or + button), `angle_rail` (a drag
  through rail fractions, then its release), `preset` (a declared aspect option, by name), `rect` (`[x, y, width, height]` in box pixels, applied as two corner gestures,
  top-left then bottom-right), `swap`, `lock`, `option`, `guide`, `apply`, `cancel`. `start` and
  `reapply` open or rebase the frame at once and wait for the crop layer's truncated input-stage
  preview under it (and a reapply for its `draft.reapply` too), `apply` waits for its committed
  pixels, and the rest are captured on the next rendered frame.
- `slider` drives one gesture on a generated control: `{"action": "set-basic", "parameter":
  "exposure", "values": [0.25, 0.5, 0.75]}` sends one pointer move per value as the slider widget
  publishes it: the rail fraction that sends that value through the parameter's declared rail
  (its soft range and fine step), the same `Fraction` message a drag produces. A value no fraction
  sends, outside the rail or off its fine grid, fails the step with each such value named. Each
  move sends its `draft.set` and the one preview job for it as soon as nothing is in flight; there
  is no tick to wait for. `"release": true` ends it with the widget's release, which commits once;
  `"cancel": true` ends it with Escape; neither
  leaves the gesture open and captures the frame once the draft has drained, so the pixels belong to
  the newest value it sent. A second `slider` step naming the same control continues the same
  gesture. An `"interval_ms": 8` field paces the values instead of sending them all at once: one
  value is sent per tick of a timer gated on the step still having values left to send, so a wild
  drag can be scripted without the harness deciding what reaches the owner. Each paced value is
  recorded as its own `slider_step_value` event (`{"value", "index"}`), even one the core's own
  gesture round trip coalesces away, so the harness can time an input that never reached the owner.
  Without `interval_ms` every value is sent at once, as before.
- `double_click` double-clicks one drafting slider's rail: `{"action": "set-raw",
  "parameter": "temperature", "value": 5000, "gap_ms": 120}`. The first press is the rail's move to
  `value`'s fraction, which opens the gesture, and its release, which commits it; `gap_ms` (0 to 250) after that
  release, one timer tick sends the reset the rail's wrapper publishes for the second press,
  whatever the commit is doing by then. The frame is captured once nothing the two presses started
  is in flight. `double_click_first`, `double_click_second` and the reset's own
  `field_reset_queued`, `field_reset_sent` or `field_reset_dropped` events record the order, and a
  refused request logs `command_failed`.
- `slider_draft` answers an open gesture's Changed elsewhere notice: `"discard"` or `"reapply"`.
- `field` types into one generated field: `{"action": "set-basic", "parameter": "exposure", "text":
  "1.5"}`, with `"submit": true` for Enter, which commits that one field without a draft.
- `curve` drives one curve editor gesture, named by `event` beside the control's `action` and
  `parameter`: `move` drags point `index` through `points` (one pointer move each) and drafts, with
  `finish` `open`, `release` or `cancel`; `add` (`point`) and `remove` (`index`) commit at once, as
  the editor's own add and remove do; `channel` (`index`) selects a channel; `points` (`open`)
  opens or closes the Points list, which sends nothing and captures the next frame; `type`
  (`index`, `axis` 0 for the input or 1 for the output, `text`) types into that point's field and
  presses Enter, which commits that one coordinate. A `type` into a closed list, a refused add or
  remove, and a typed value the kind refuses fail the step with the status bar's text.
- `reset` runs a declared reset: `{"module": "luxforge.basic"}` is the module's own header reset and
  `{"module": "luxforge.basic", "group": "Tone"}` is that control group's, found by its label.
- `pick` clicks the photograph at a pixel of the raster on screen: `{"x": 120, "y": 80}`. What the
  click does is the active canvas mode's own declared pick, so a `workspace` step selects the mode
  first; a mode that declares none fails the step. A pick that commits is captured on the render it
  produces, and one that is refused on the status it leaves.
- `view` sets the zoom through `view.set`: `{"zoom": "fit"}` or a percentage from 10 to 1600, as a
  number.
- `pan` scrolls the percent-zoom canvas to a fraction of its scrollable range on each axis:
  `{"x": 0.5, "y": 0.5}` is the centre and `{"x": 1, "y": 1}` the far corner. It goes through the
  same scrollable a Space drag scrolls, and is captured once the offset the scrollable reports has
  reached the session through `view.set`, so `state.surface.view` carries the pan the frame was drawn
  at. At Fit there is no scrollable and the step fails.
- `key` (`{"key": "w"}`, one letter or digit, or `"Escape"`) presses one key with no text field
  focused, through the desktop's own key table: a key that enters a canvas mode is captured once the
  session follows, a Select key (`g` among them) once Select has nothing in flight, any other bound
  key on the next frame, and a key the table does not bind fails.
- `select` is one gesture on the Select workspace, sent through the message its control or the key
  table sends: `{"switch": "select"}` or `"develop"` presses the title bar's workspace switch;
  `{"source": "Konstanz · 12–13 Sep"}` presses the source row showing that name, or that name and its
  dates; `{"arrow": {"direction": "right", "extend": true}}` presses an arrow key through the key
  table, Shift held when `extend`; `{"choose": {"menu": "group", "item": "Day"}}` opens the Camera,
  Kind or Group chip's menu or the sort's (`camera`, `kind`, `group`, `sort`) and chooses the item
  with that label, or presses a pick segment (`pick`); `{"click": {"position": 5, "shift": true}}`
  presses the grid cell showing that view position, with Shift or Command (`command`); in the loupe
  an `arrow` steps its frames (`left`, `right`) and moments (`up`, `down`), and a `key` step's `e`,
  a digit, `z`, `c`, `p` and `Escape` are the loupe's, each captured once the frames on screen are
  decoded at their size and the focus check's region has landed; and
  `{"agent_pick": {"positions": [5]}}` has the run's second client (as for `agent`, actor
  `evidence-agent`) pick the files at those positions with `pick.set` (`"picked": false` clears
  them); `{"folder": "/path"}` browses that folder as Browse a folder… does, bypassing only the
  native dialog; `{"library": "undo"}` presses `Cmd+Z` through the key table (`"redo"`:
  `Shift+Cmd+Z`); `{"pick_all": {"position": 12}}` scrolls the grid to the bracket holding that
  view position and presses its header's Pick all; and `{"add_folder": "/path"}` adds that folder
  as Add a folder… does, bypassing only the native dialog, scrolls the sources panel to On disk,
  and is captured once its listing has ended and the indexed folders have been read again, with
  the owner's own `index.folders` recorded as `owner_indexed`. `P` is a `key` step. Each is captured once
  nothing Select asked the owner for is in flight — the events, the cards and volumes, the catalog's
  counts, the view, its facets, a staleness check, a change's label, a folder listing and the rows
  near the screen — so a pick, Pick all, undo or redo is captured with its view evaluated again,
  and an `agent_pick` only once
  the desktop has evaluated its view again, which it learns of through its own event sync; switching
  to Develop is captured on the next frame, and a folder once the index lane has read it and it is
  viewed. A settled step records `owner_browse`, the owner's
  `session.state` `browse` block for the desktop's client, so a frame's selection can be checked
  against what the owner holds. A source row that opens the native folder dialog, a menu item that
  does not exist and a position whose row is not read fail the step.
- `loupe` is one gesture on the open loupe that its timing needs: `{"arrows": {"direction":
  "right", "count": 30, "interval_ms": 30}}` waits until the look-ahead is warm — every frame the
  loupe wants, on screen and ahead, decoded at its size or with nothing more to wait for — records
  `loupe_warm`, then presses the arrow `count` times (1 to 240) through the key table,
  `interval_ms` apart (1 to 1000; a single press takes none), the first a press and the rest the
  key's repeats, and is captured once Select has nothing in flight after the last;
  `{"pointer": [0.3, 0.3]}` moves the pointer over the picture to those fractions of it, recorded
  as `loupe_pointer_sent`, and is captured once Select has settled, with the focus check on once
  the region under the pointer has landed. A closed loupe fails the step. Wherever events are
  written the loupe records what a timing harness pairs: each key that moves the active frame
  (`loupe_key`, with `pressed_ms` from the start of its handling, where it moved from and to and
  whether that frame was already `ready`), `Z` (`loupe_focus`), each region asked for
  (`loupe_region_asked`), and, from the model just derived, each picture the active frame presents
  (`loupe_presented`, under its own item and preview key, a stand-in said so) and each region the
  inset presents (`loupe_region_presented`). Presented means the update whose redraw draws it, as
  for `preview_displayed`, not scanout. Each frame's `select.loupe.frames` block also records the
  most decoded bytes the loupe has held at once (`peak_bytes`), kept when its frames are released.
- `grid_scroll` (`{"px_per_frame": 60, "frames": 240}`, 1 to 2000 logical pixels on each of 1 to
  1000 frames) scrolls the Select grid down on each frame of the window's own frame clock, which it
  subscribes to only while it scrolls, sending the offset the grid's scrollable publishes, as a
  steady trackpad scroll does. Each frame is recorded as `grid_scroll_frame` (the frame's time, the
  offset, the cells on screen drawing a decoded preview, the placeholder while one loads, or
  nothing ever, and the grid's decoded preview bytes against its budget), and the grid records
  `select_scrolled` in the update that adopts an offset, the one whose redraw draws it. It is captured once Select has nothing in flight after its last frame,
  or after the frame that reached the end of the grid; a grid that is not shown fails the step.
- `missing` is one gesture on Select's Missing originals, sent through the message its control
  sends: `{"find": {"group": "2026-09 Konstanz", "folder": "/path"}}` presses Find in a folder… on
  the group developed from the folder of that name and answers the folder dialog with `folder`,
  captured once the search has ended; with `"stop": true` Stop search is pressed as soon as the
  search is running (the step records `stopped_job`, and fails if the search ended first), captured
  once its job has ended; `{"filter": "needs_you"}` presses a filter segment (`all`, `found`,
  `needs_you`, `not_found`); `{"row": "DSC_0101.JPG"}` selects the row of the photograph whose
  original has that name; `{"choose": {"file": "DSC_0107.JPG", "index": 1}}` opens that row's
  Choose… menu and chooses its file at `index`; `"relink"` presses Relink N (the step records
  `relink_pairs`); and `{"locate": {"file": "DSC_0108.JPG", "path": "/path"}}` presses that row's
  Locate… and answers the file dialog with `path`. Each is captured once nothing Missing originals
  asked the owner for is in flight — the list, a search, a Locate, a relink and the selected row's
  facts. A step fails when Missing originals is not shown, or the group, row, action or menu file
  it names is not there or is refused. Each frame's `missing` block records the groups as
  `source.missing` answered them, each search's rows as `source.find` answered them with paths
  under the searched folder, what the view draws, the pairs Relink would send and the Info panel.
- `catalog` is one gesture on the catalog in Select, sent through the message its control sends, as
  the view model offers it: `{"source": "Real photographs"}` presses the catalog folder, year or
  collection row of that name; `{"search": "Luzern"}` types that text into the search field;
  `"metadata"` presses the Metadata chip; `{"facet": {"column": "camera", "value": "iPhone 15
  Pro"}}` presses that value of a Metadata browser column (`date`, `place`, `camera`, `lens`);
  `{"edited": "Not edited"}` chooses from the Edited chip's menu; `{"rename": {"folder": "A",
  "name": "B"}}` chooses Rename… from the folder's menu, types the name and presses Return;
  `{"nest": {"folder": "A", "into": "B"}}` and `{"merge": {"folder": "A", "into": "B"}}` choose
  Move to… or Merge into… and the folder listed as `B` (`Top level` for Move to…); `{"new_folder":
  "A"}` presses the Catalog heading's `+` and New folder, types the name and presses Return;
  `{"move_to": "Bodensee"}` and `{"add_to": "Print order"}` press the Info panel's Move to… or
  Add to… and choose the folder or collection listed so, for the selection; `{"save_smart":
  "Name"}` presses Save as smart collection…, types the name and presses Return; `{"apply_preset":
  "Warm"}` opens the Develop band's Apply preset… and chooses that library preset, and
  `{"export_into": "/path"}` presses Export… and answers the folder dialog with that folder, each
  captured once its batch has ended and what it changed has been read again; `"report"` presses
  the status bar's Report; `"remove"` presses the Info panel's Remove from catalog…, `"delete_key"`
  presses ⌫ through the key table and `"empty_removed"` the filter bar's Empty Removed…, each
  captured with its confirmation; `"confirm"` presses the confirmation's button; `"put_back"`
  presses the Info panel's Put back; `"send_back"` presses its Send back, failing with the reason
  when it is refused; `{"context": 5}` right-clicks the grid cell showing that view position,
  opening the selected photographs' menu; and `{"context_choice": "Send back"}` chooses from it. Each is captured once nothing Select asked the owner for is
  in flight and no batch or emptying of the desktop's runs (a gesture that asked for nothing, such
  as the Metadata browser opened over counts it holds, on the next frame), so a library change is
  captured with its view and lists read again. A row, column value, menu choice or refused choice
  that is not there fails the step, naming what is. Each frame's `select.catalog` block records the
  folders and collections listed, the filter bar, the whole query shown, the owner's facets and
  the Metadata browser's rows, the source's total, the Info panel's organize and Develop bands, the
  last batch's request, its `job.read` record and the report shown, the sheet over the centre, and
  each `catalog.empty-removed` call with its answer.
- `wait` (`{"ms": N}`, 1 to 10000) asks nothing of the editor for at least that long and then
  captures. The evidence run's own 250 ms tick keeps rebuilding the view meanwhile, so the frame
  shows what repeated rebuilds with nothing new to show did.
- `gpu_warmed` (`{"quiet_ms": N, "ms": M}`, N at most 10000 and M from N to 60000) asks nothing of
  the editor for at least `quiet_ms`, then until the GPU preview's compile thread has compiled
  everything handed to it — the desktop's newest warm list taken (`gpu_preview_warmed` equal to
  `gpu_preview_warm`) and `gpu_preview_compile_pending` zero — or until `ms` from the step's
  start, and captures. Its record's `gpu_warmed` says whether it `finished`, how long it waited
  and what was still pending.
- `workspace` sets any of `state_panel`, `tools_panel`, `mode`, `thirds`, `clip_shadows` and
  `clip_highlights` through `workspace.set`, naming only the fields that actually differ from the
  session's own; captured on that round trip, or immediately when nothing differs. A step that
  switches a clipping overlay **on** waits instead for that overlay's own bounded texture to reach
  the GPU, so its frame shows the mask rather than the photograph a moment before it.
- `preview` selects a loaded history entry by its sequence number (`{"sequence": N}`) or returns to
  the current state (`"current"`), exactly as a history row's click or "Return to current" does.
  Captured once its pixels reach the GPU.
- `palette` opens the command palette and types a query: `{"query": "text"}` captures the next frame
  with the palette open; `{"run": "text"}` additionally runs the first matching entry, settled the
  way that entry's own message would be.
- `canvas_hover` (`{"x": 0.5, "y": 0.5}`) routes a native cursor move through the laid-out widgets,
  using fractions of the visible photograph. `canvas_hover_sweep` takes `points` and `interval_ms`:
  at most 240 positions, 1–1000 ms apart, and at most 10 seconds total. Overdue positions coalesce
  before dispatch; the result records emitted/coalesced inputs, cursor geometry and matched editor
  timings. Each step captures its final native frame. These measure CPU geometry submission and
  renderer readback, without a GPU-completion or display-scanout claim.
- `preset` clicks one row of the Presets section: `{"name": "Soft film", "group": "Synthetic"}`.
  The name matches exactly, case included, and `group` is needed only when two groups hold that
  name; no match, or more than one, fails the step. The click is the section action's own
  `ActionMessage::Run`, so the frame is captured on its pixels like an `api` step's.
- `preset_create` fills the create form through its own messages and presses Create:
  `{"name": "Tone only", "group": "User presets", "groups": ["Basic · Tone"]}`, where `groups`
  lists exactly the checkbox labels to leave checked and `group` defaults to the form's. With
  `"submit": false` the form is left open and filled, and the next frame shows it. A submitted
  form is captured once the library answers.
- `preset_delete` opens a row's menu and chooses Delete: `{"name": "Tone only"}`, matched as
  `preset` matches. Captured once the library answers.
- `preset_import` imports one file through the section's own import task, bypassing only the native
  dialog: `{"path": "fixtures/presets/develop.xmp"}`, relative to the editor's working directory.
  Captured once the library answers; a refused file is a failed step.
- `performance_cancel {row}` presses Cancel on a displayed running Performance job row, numbered from zero, and captures its command answer; a row without an enabled Cancel fails the step. The `capabilities` scenario cancels a real held task through this button and verifies that its accepted edit remains intact.
- `performance` opens or closes the state panel's Performance section through its heading's own
  message: `{"expanded": true}` or `{"expanded": false}`. Opening it, with the state panel shown, is
  captured once the section's first `resources.read` and `activity.list` have answered, so the frame
  shows figures rather than dashes; closing it, opening it under a hidden panel and asking for the
  state it is already in are captured on the next frame. The section is open at every launch, so
  every scripted run samples once a second unless its script closes the section.
- `settings` opens the Settings sheet at a tab, as its title bar button and tab rail do, or closes
  it: `{"open": true, "tab": "appearance"}`, where `tab` is `general`, `appearance` or
  `experiments` and Experiments when left out, or `{"open": false}`. Opening is captured once
  `flags.list` has answered; closing, moving an open sheet to another tab and asking for the state
  it is already in, on the next frame.
- `flag` changes one flag through its row's own control while the sheet is open:
  `{"id": "proof.number", "value": 75}`, or `"value": null` for Reset. A toggle takes a boolean and a
  choice one of its options; a number is typed into its field and Enter pressed. A change the row
  sends is captured once `flags.set` has answered; a number the field refuses sends nothing and is
  captured on the next frame with the refusal in the status bar. A flag the sheet does not show, a
  value its control does not offer and a Reset with nothing stored fail the step. `state.json`
  carries a `settings` summary: the tab, the flags as last listed, every row as drawn and the writes
  outstanding.
- `theme` chooses a theme as its Appearance row does, by id or, for an import whose id is new on
  every run, by name: `{"id": "omarchy.nord"}` or `{"name": "Linen"}`. It is captured once the
  theme is drawn, or Luxforge Dark in its place with the reason in the status bar, and the
  preference writer has stored the choice; a theme the library does not list fails the step.
- `theme_import` imports one Luxforge theme document through Import theme file…'s own task,
  bypassing only the dialog: `{"path": "fixtures/themes/paper.lftheme"}`. `theme_import_omarchy`
  imports an Omarchy theme folder, or a folder of them, through Import Omarchy theme…'s task:
  `{"path": "fixtures/themes/omarchy"}`. Both are captured once every `theme.import` and the
  listing after them have answered; the Omarchy step records what each theme became
  (`folder_import`, with each import's report) and fails only when the folder is refused as a
  whole, since a conflict or a theme that fails is the import's own outcome. Every frame's
  `state.json` carries a `theme` summary: the theme drawn and the choice it answers for, its mode,
  why a choice is not drawn, its generation, its surround, background, surface, control and text
  tokens, the library as last listed and the last folder import's outcomes.

- `capability` drives one gesture on a module's task control or the consent notice through the message that control sends: `{"module", <one of>, "wait"?: false}` with `task` (`{task}`), `consent` (`"allow"` or `"deny"`), `apply` or `settle`. A module's settings, profiles, secrets, grants and resources are set with `api` steps. A step is captured once its round trips have answered and the jobs it started have finished; `"wait": false` captures as soon as a started job reports progress, and a later `settle` captures once the module's jobs are done. `state.json` carries a redacted `capabilities` summary per module — the settings as `module.settings.read` answers them (a secret only as `secret_present`), each resource's state, the jobs, each task's newest run, the permission counts, the open consent and the block's status line — and stack layers carry their `artifacts`.

A step that cannot be sent is recorded with `"status": "failed"` and its reason and still captures a
frame, so a refused step is visible in the evidence instead of missing from it.

Each scenario's plan and checks are documented in its own module (`xtask/src/*_smoke.rs`, with the table of rows in `xtask/src/smoke.rs`), and `cargo xtask smoke --list` names every scenario with what it proves, the launches and frames it makes, what it opens and its window. Most write the values they measured and their tolerances to a `*-checks.json` file in the run's output. A scenario's script is the steps above, so what a step does is specified here and what a scenario asks of it is in the scenario. The gallery board and the controls scenario's generated panel are different widths; both need visual review alongside their automated checks.

`editor-latency --control curve` drags point 1 of a curve with the curve editor visible, in drag or commit mode only. Without `--action` it is the developer controls proof's middle point, in a developer launch with the proof section expanded and the tools panel scrolled to its end; the proof's colour stage is identity, so this measures the control, query, draft, preview and upload path and no image algorithm. `--control curve --action set-curve --parameter luminance` measures the Tone curve instead: the action must be a registered, non-developer field-patch action and the parameter one of its curve parameters, so a number parameter is refused with `--control curve` and a curve parameter without it, each by name. The ordinary launch commits `[[0, 0], [0.5, 0.5], [1, 1]]` through the action first (naming `Mask 1` with `--mask`), so point 1 is a mid-tone point rather than the white point, collapses every section the panel lists above the module, expands the module's and scrolls the panel to its top, and checks that frame for the seeded, sampled curve before timing; every drafted frame then runs the curve's colour unit. Slider remains the default workload. Every workload reports against the same provisional input-to-presented-frame target (16 ms p95, acceptable below 32 ms) and retains all samples.

`detail-performance` complements the desktop measurements with the public core renderer and catalog
API, including the owner's tile service and export lane. It requires a release xtask build, accepts only the
planned 24 MP and 60 MP JPEG workloads, defaults to 30 samples and takes the host-wide timing lock.
Run each source sequentially on a quiet host after integration code and relevant native functional
checks are finished. Outstanding photographic gates or an unavailable display density stay
unqualified; scoped measurements at the tested density can proceed with that limit recorded. No
timing runs overlap builds, tests or another plan's measurements.

```sh
cargo run --release --locked --package xtask -- detail-performance --source fixtures/generated/24mp.jpg --output NEW_24MP_DIR --samples 30
cargo run --release --locked --package xtask -- detail-performance --source fixtures/generated/60mp.jpg --output NEW_60MP_DIR --samples 30
```

`--case render|export|points|sharing` isolates a workload in a fresh process/output directory;
`all` is the default. The helper commits luminance 25, colour 25 and sharpening 40 plus Basic
exposure +0.5 EV. Full render includes compilation, while export measures request-to-ready,
quality-90 encoding and durable publication with metadata off. Decode/preparation is outside these
timers. The points case reopens and prepares an owner for every sample, then measures its first and
second 25-point neutral query. Warm means the second request on that owner; each query is one call
to the owner's reference tile service, which renders the prefix's frame for it. It also measures the
first colour-limited brush tick through the same service and a later tick using the same draft memo. Sharing checks allocation identity for prepared
jobs and default/ancillary-only Detail; it is a functional check without a latency distribution.

`result.json` retains every observation, nearest-rank distributions from `luxforge-testbase`,
executable/source/lockfile hashes, git identity, recipe, run identity and host load. It writes failed
status explicitly and preserves completed cases. `resources` separates exact colour/spatial budget
counters from OS process memory. OS high-water marks include earlier cases in the process, so use
isolated `--case` runs for attribution. Private cache counters, GPU residency and backend staging
remain unmeasured. This helper launches no editor and
does not measure native presentation, RAW residency or photographic quality. The generic `timing`
tier does not run this Detail matrix automatically.

`detail-grid-performance --source JPEG --output NEW_DIR --samples 30` requires a release build and
the exact planned 24 MP or 60 MP JPEG dimensions. It measures value-mask input grids through a public
`Latest` worker and its per-worker input-grid cache. It calls the production
coverage evaluator for a 2880×1800 dense overlay and a 28×19 sparse thumbnail, pairing a first grid
build with reuse after changing only `mask.amount`. The report keeps generation, evaluation and
source identity, grid keys/hashes/cell counts, publicly reported cache bytes and resource budgets.
These are headless coverage timings; private hit counters, GPU presentation and backend staging
remain unmeasured. It is a separate command with the same quiet-host timing lock and is not run by
the generic timing tier.

```sh
cargo run --release --locked --package xtask -- detail-grid-performance --source fixtures/generated/24mp.jpg --output NEW_GRID_DIR --samples 30
```

Desktop `editor-latency --detail` commits sharpening 60, luminance 40 and colour 40. Paint commits
that global layer before selecting its brush mask and verifies it in the captured stack. The
`--idle` workload reopens the gesture catalog with Detail preserved, settles and samples 30 seconds
of idle CPU with the worker caches held. Its idle process has no frame captures,
so its private cache bytes and scratch counters are unavailable. Paint and hover use an unlimited
brush, which reads no input pixels: their overlay/coverage figures do not exercise the value-mask
input grid's build or reuse. Use the separate value-mask grid
workload above for that scope. Commit-to-frame and
commit-to-histogram rows describe release. Preserve all of these gaps in the performance task and evidence record.

`editor-performance --lens-only` accepts JPEG or RAW. It prepares one source before measurement,
queries the first eligible profile, freezes its terms and compares exact full renders with Lens
neutral and selected while holding Perspective at +20/-10 and an original-aspect crop at 2.5°.
Each selection sample resets Lens first, so the timed selection must make a durable change rather
than return a no-op. Query, selection, render, a centre-point `render.sample`, serial JPEG export
completion and immediate export cancellation each have their own distribution. Completed files
are removed outside the timed window; cancellation must reach the cancelled terminal state
without leaving a destination or staging file. The report records both render identities,
dimensions, frame hashes, the exact point result, source preservation, scratch high-water and
process resources. Baseline and corrected renders alternate order over the same warm prepared
source. This is a comparison within the current build; it excludes decoding, desktop scheduling,
GPU upload and presentation. Cancellation timing starts at `job.cancel`, after export admission,
and terminal-job polling has a 2 ms interval.

`editor-latency` is the desktop counterpart to `editor-performance`, which measures exact core
rendering over cached sources and so cannot see desktop scheduling, GPU upload or presentation. It writes its own
evidence script, drives the release binary through a background evidence launch, and reads the
timings out of that run's `events.jsonl`. Each measured input is one scripted `slider` step left
open, so the step settles only once the gesture has drained: one input, one `draft.set`, one frame,
with nothing from the previous input still in flight. `slider_draft_set` gives the input's time. A
tick the GPU stage draws ([GPU previews](../design/gpu-preview.md)) is answered by the
`gpu_preview_tick` of the same update, which names its draft and revision and queues no preview job,
and its step settles on that tick rather than on a CPU frame. A tick on the CPU path is answered by
`slider_draft_preview`, which names the preview generation, after the `gpu_preview_tick` that says
why it took that path (`boundary-pending` for a gesture's first tick when no resident boundary has its key, which asks for the boundary;
`compiling`; ...). A tick that holds its frame for a reason that passes (`gpu_preview_tick` with
`path: "held"` and its `reason`) answers its set too, as a GPU input of its revision marked with the
reason: it is presented by the surface's first draw of that revision's plan, a later `gpu` tick of
the same revision answering no set, or, once the hold lasts, by its reference frame
(`gpu_preview_reference`, then that generation's `preview_displayed`). A held input a later input
superseded before either is reported, not failed; a set no tick answers still fails the run.
`held_input_to_presented_frame` and `unheld_input_to_presented_frame` split the inputs by whether
their tick held, `paths.held` counts the held inputs by reason and by how each ended (drawn on the
GPU, presented by its reference, superseded) and `frames` names each one's `held` reason; the
verdict is on all inputs. A CPU frame is presented by the `preview_displayed` of its generation,
the update in which its raster became the photo surface's source. A GPU frame is presented by the
surface's first draw of the plan tagged with the input's draft revision: the surface stamps each
frame it first draws, and the desktop logs the stamp as `surface_frame_drawn` with the draw's own
instant on the run's clock, its path, and its draft and revision or the generation of its CPU
picture; a GPU frame's `evaluation` says what the evaluation in the frame that first drew it did
(`refits`, `rebinds`, `links_run`, `spatial_passes`, `lights_encoded`, `lights_restored`,
`window_texels`, `link_texels`), which a slow tick is attributed by. Neither is display scanout: the figures are an upper bound on the editor's own work and a
lower bound on what an eye sees. `input_to_presented_frame` pools both paths, which
`gpu_input_to_presented_frame` and `cpu_input_to_presented_frame` split; `input_to_drawn_frame`
times both to the surface's first draw; `gpu_tick_to_drawn_frame` runs from the GPU tick's update to that draw.
The report has no upload row: the photo surface writes its texture in the frame that draws it, so
`render_and_upload` covers a CPU frame's render and hand-over together. `frames` lists every
drained input with its path, reason, draft, revision or generation, its two figures, and what the
frame captured after its step showed: a GPU input's capture must show the GPU path at its own
revision, or the run fails. `paths` counts the inputs by path and the CPU ticks by reason, beside
the whole run's ticks by path, and `compile_queue_before_gesture` the GPU stage's programs handed
and compiled before the first input. `--warm MS` (drag mode, up to 10 s) waits that long after the
preconditions, so a committed stack's programs finish compiling off the interface thread as they
would before a person's next drag; without it the first ticks over a Presence or Detail layer can
take the CPU path as `compiling`. The measured window is
invisible, so nothing in these runs is composited or scanned out at all; the figures cover the
editor's own path to the texture and say nothing about the cost of putting that texture on a
screen. The last scripted value also
releases, so its drafted preview is superseded by the commit — that is the queue cancellation the
report counts — and it is measured through to the `analysis_adopted` of the committed frame, which
is the settled exact histogram, and to that frame's own first `preview_displayed`
(`commit_to_committed_frame`), which is what a person sees on release. A final burst step sends
every value between two ticks to show the driver's coalescing. A RAW temperature or tint drag is
timed like any other: its drafted values are drawn from the planes developed at the committed white
balance, the report counts the CPU frames among them that approximate it in
`approximate_white_balance_frames`, and its releases, which wait
for the mosaic to be redeveloped, show in `commit_to_committed_frame`. An input whose preview job
was refused — logged as `slider_draft_unpreviewed`, a RAW draft whose development is not in memory
because a redevelopment is in flight — has no frame of its own, and a drag with one is refused with
that reason rather than timed. A RAW slider's gesture values start from zero when its range holds
it and from its declared default otherwise (Custom temperature's 6504 K); `--mode burst` takes
`--action`/`--parameter` too. `--lens` selects the first eligible profile through the Lens control,
carrying the assume-uncorrected acknowledgement explicitly. `--perspective` commits +20 horizontal
and -10 vertical before measurement. `--crop DEGREES` commits a straightening 16:9 crop first, so
the measured stack carries the fused geometry resample as well as the colour pass. These flags
apply to drag, commit, burst, paint, hover and crop-start. `--basic` commits a Basic layer
with every field non-neutral first, so each measured frame runs every one of the module's colour
units; over a RAW source it leaves out Temperature and Tint, which are the source development's
there. `--contend N` (drag mode, 1 to 5) queues N `export.jpeg` jobs of the committed stack just
before the first input, which the GPU tile worker streams in tiles on the window's adapter while the
drag draws, as a person's exports run beside their next edit, and reads the
export lane back with an `activity.list` step after the release: the lane runs one job with the rest
waiting, so it is busy from the first export's acceptance to the last one's end, the answer itself
while one still runs. An input sent in that window is contended (`frames[].contended`), and
`contended_input_to_presented_frame` is their figure; a run with none fails. The window spans the
exports' encodes and writes as well as their tiles, and the exported files are removed after the
run. How much of a drag the exports cover depends on the source's size, so `frames[].contended`
says which inputs they covered. Each export's `export_finished` event in the run's
`events.jsonl` carries the tile worker's figures (`tiles`): beside its counts and bytes,
`last_tile` and `tiles_total` split its tiles' time on the worker's thread into `light_ms`,
`upload_ms`, `encode_ms`, `wait_ms` (the wait for its device, an upper bound on the GPU's work) and
`read_ms`. `--idle`, in drag mode, first closes the Performance section
after the release, whose one-second sampler would wake the editor, and runs an `idle` evidence step
in the gesture's own launch: a 4-second settle for the release's dissolve and the picture at rest, then a 10-second window over which the surface's drawn frames, the views built and the
process's own CPU time are counted (`idle_after_dissolve`, and its
`idle_after_dissolve_process_cpu_percent_one_core`, `_drawn_frames` and `_views` rows; the window's
own start counts one frame and one view). The run fails unless a dissolve ran before it. `--idle`,
in drag and commit modes, then reopens the gesture's own committed catalog
and source in an ordinary launch. After the exact histogram is adopted and one second of settling,
it measures CPU time and sampled RSS for thirty seconds with the same Detail, Lens, Perspective, crop and
colour stack. This includes the open Performance section's sampler. The harness then stops the
process; this is no clean-close check. `latency.json` and `resources.json` keep every sample,
scratch high-water and correlated state. Native GPU allocation, when the platform provides it,
and the photo surface's full, retiring and crop-stage texture bytes are captured levels
from the gesture, not idle-process measurements or peaks. The GPU preview stage's own budget is
reported beside them: `gpu_preview_peak_bytes`, the most its slots have held over the run, which is
its high-water, with `last_gpu_preview_in_use_bytes`, `last_gpu_preview_scratch_bytes` (the part
that is the slots' shared scratch pools) and `gpu_preview_budget_bytes`. `measure`
drives no gesture, so it has no GPU preview figures. Surface bytes exclude backend staging;
invisible windows provide no compositor or scanout figures. Missing counters remain null.

`--mode burst` is a wild, undrained drag rather than the drained gesture drag and commit mode
measure: one scripted `slider` step, paced through `interval_ms` at 120 values a second for 3
seconds (360 values, a triangle wave about the field's origin peaking at 40% of the smaller half of
its declared range, on its own step: from 0 to +2 EV, down to -2 EV and back to 0 on an exposure
slider, 6500 K up to 8300 K, down to 4710 K and back on Custom temperature; released at the end) instead of sent all at once, so
the desktop's own gesture round trip decides what reaches the owner exactly as a real fast drag
would. `--samples` is ignored: every burst run
sends the same fixed values. Its `latency.json` keeps the same header fields as drag and commit
(host, binary hashes, launch mode, method, queue counts) and adds a `burst` object: `scripted_values`
and `sent_values` (the paced driver's own `slider_step_value` events, which count a value the core
coalesces away as scripted rather than measured), `draft_sets` and `preview_jobs` (the core's own
real-time coalescing of that pace), `presented_frames` (every `preview_displayed` and every first
draw of a GPU tick's plan over the run, drafted and committed alike), `gpu_frames` and `cpu_frames`
(the drafted inputs that reached the screen, by the path that drew them), `cancelled_exact` (`preview_exact_cancelled` events: full-resolution
phases a newer request superseded, which carry no frame). Its figures are rows: `presented_fps` (`presented_frames` divided by
the seconds from the first `slider_step_value` to the last of them), `staleness_ms` (each presented
drafted frame's own `slider_draft_set` time to its presentation, paired by generation or by GPU
draft revision exactly as drag mode pairs them), `frame_gap_ms` and `max_gap_ms` (the intervals between
consecutive presented drafted frames), the GPU counters `draw_encoded_frames`,
`photo_texture_writes` and `photo_upload_bytes`, and the sampled resources.
With `--zoom PERCENT --moving-pan`, the same paced tick also moves the photo scrollable on a path
across and back over the image. Without `--moving-pan`, a zoomed burst holds a fixed viewport and
its script can run against the pre-viewport binary for a like-for-like baseline. The moving-pan
report counts `slider_step_pan` events alongside the captured surface GPU counters. A burst
still measures adoption rather than scanout; multiple adoptions can occur before one draw.

`--zoom PERCENT` sets the view before the measured gesture.

`--mode paint` measures a **paint** gesture instead of a slider, because a stroke is not a field patch
and the slider modes cannot drive one. It builds the recipe the figure is about — one brush mask
of one component, seeded by one stroke, and one masked Basic exposure layer, asserted in the captured
state rather than assumed, plus any requested global Detail, Lens, Perspective and crop — and then paints one stroke
whose positions are handed to the desktop one
per 24 ms in real time: the first tick presses, each later one moves and the last releases, so one
paced step is still one stroke and one history entry. `--samples` is the number of positions, 2 to 1000.
Each position is its own mask `draft.set` and frame: on the CPU path a preview job and its displayed
frame, paired by the generation `mask_draft_preview` carries; drawn on the GPU, the
`gpu_preview_tick` of its own update and the surface's first draw of that tick's plan
(`surface_frame_drawn`), listed in `gpu_samples` with the positions its `draft.set` carried. A
position whose frame a later one superseded is reported as `superseded` rather than averaged away.
`input_to_presented_frame`, the press rows and the position rows count both paths;
`gpu_input_to_presented_frame`, `cpu_input_to_presented_frame` and `gpu_tick_to_drawn_frame` split
them, and the per-phase and quartered phase rows are the CPU frames' own. `latency.json` records the recipe it was painted on, the brush,
the path, the distributions as rows and, in its `load` record, the one-minute load average at the
start, marked `unreliable` above the 8.0 threshold (the end-of-run load is recorded beside it). It exists because the `mask-range` scenario's paint figure is taken on four masked
colour layers, three of whose masks bind the whole stage, and is therefore not a baseline for the
gesture; the pairing itself is one function shared with that scenario so the two cannot drift.
This mode's own paced stroke leaves the step's `settle_between` field unset: it is a controlled,
dedicated run of one host rather than a smoke scenario sharing it with whatever else is running, so
`PAINT_INTERVAL_MS` alone stays the measurement's own definition.

A long trajectory can exceed the bounded 4096-event diagnostics log before reaching the
1000-position grammar limit. Such runs fail explicitly and provide no final/late timing result.
The masking qualification uses 240- and 400-position holds; capture-bound tests qualify storage
limits separately from native latency. See [masking measurements](../specs/performance.md#native-masking-interaction-qualification).

The separate `input_to_authoritative_mask_coverage` row pairs an accepted draft ID/revision with
the exact coverage that reached the presenter. Reused photograph pixels are never counted as a
new render. `--mode hover` prepares a masked Clarity +50 adjustment by default, arms New Mask and
sends a native path every 16 ms without pressing. It reports cursor geometry, pointer/editor work,
that no point query was asked, coalescing, photo writes and sampled RSS. Use
`--action set-basic --parameter exposure` for a masked +0.5 EV control; `--zoom`, `--crop` and
`--basic` exercise the same route in other views and stacks. Hover takes 1–240 scheduled inputs;
the emitted count is the distribution's sample count.

`--mode crop-start` measures opening a crop draft at Fit. It commits the recipe the flags ask for
(`--detail`, `--lens`, `--perspective`, `--crop`, `--basic`, and `--presence` for a Presence layer with all three fields at 100), opens the
Performance section, settles for 1.5 s, then per sample Starts a crop draft, holds it open for 1.5 s,
cancels it and settles 1.5 s more; `--samples` is 1 to 14 and `--zoom` is refused. The complete
script must remain within the 64-step evidence bound, including all preconditions; fourteen samples
with Lens, Perspective and crop use 63 steps, or 64 with Detail too; additional preconditions need fewer repetitions. From events every binary logs it reads
the Start step's `script_step` to `crop_draft_started` and to the `frame_captured` of that step, which
the editor captures once the crop layer's input stage is on screen, so that figure includes the
window readback. The Performance section's `resources.read` memory and GPU allocation are read from
the frame captured at the end of each hold, beside the settled baseline before the first Start;
`latency.json` also lists each Start's stage frame and the surface's `stage_resident_bytes` where
the binary reports them.

The `mask-range` scenario's own paced stroke, by contrast, sets `settle_between: true` on its
`mask` stroke step: the desktop then holds every position after the first until the position before
it has its own frame on the screen, instead of trusting `STROKE_INTERVAL_MS` alone to outrun the
render pipeline. On a quiet host this changes nothing the eye would notice — the wait is already
satisfied by the time the next tick fires — but on a heavily loaded one (failures were recorded
at a one-minute load average of 27 to 48) it stretches the stroke's real time instead of letting a
later position supersede a drafted frame before it ever reaches the screen, which used to leave the
scenario's `stroke_latency` check with no pair to measure at all. The check that a fully unpaired
stroke fails the scenario is unchanged and still fires for a genuinely broken stroke; what changed is
that a busy host no longer produces one.

On macOS, smoke, hardening, measurement, latency and RAW editor subprocesses always use the same background bundle as `develop --background`, and every one of them that launches the editor passes `--hidden-window`, so the run has neither an activated process nor a window on screen. Reports record `launch_mode`; reproduce through the harness to preserve focus protection. Smoke, `editor-latency` (every mode), `measure` and `hardening` make every editor launch through one envelope, `xtask/src/scenario/launch.rs`, which assembles each argument list, waits on each process through one watch loop (each tool keeps its own poll rate and deadline) and writes the run's `result.json` and `reproduce.md`: `result.json` lists every editor process the run started under `launches`, with its command and exit code, a launch the harness ends itself (an ordinary launch it has finished watching, such as an idle process) listed as `stopped`, and that list is what `verify` counts. Each tool's own report (`latency.json`, `measurements.json`, or `hardening`'s `result.json`) carries one provenance header: `launch_mode`, `platform` (the Rust host triple), `profile` (read from the `debug_assertions` the binary itself reports in its `startup` event), `binary_sha256` and `lockfile_sha256`. A native graphical session is still required. `LUXFORGE_BACKGROUND_BUNDLE_SUFFIX=SUFFIX` (letters, digits and hyphens) gives the run's bundle the identifier `org.luxforge.background-test.SUFFIX`, whose per-application caches, Metal's compiled shaders among them, start empty for a new suffix: the way to launch on a cold shader cache without clearing any cache. Windows and Linux retain direct launches; background behavior is not claimed there. Measurement launch times include the temporary bundle and executable copy, so they do not measure normal foreground activation, and with an invisible window they do not include the cost of compositing a visible one either.

That an automated launch never takes the desktop is proven once, not per run: `hardening` reads the frontmost application's pid through LaunchServices (`lsappinfo`, no Automation permission needed) while its abrupt-termination child is running and fails if that pid is the editor's, recording it as `frontmost_pid_while_running`. Every other run relies on the bundle and the hidden window and does not re-measure the desktop, so switching applications or locking the screen during a run does not affect it.

Rules for any UI or image check:

- Capture after the intended generation is rendered, tied to state and logs, with explicit provenance. A PNG's existence is not a pass. At Fit, evidence also waits for any permitted display-bounds refit. Drafts that deliberately defer refitting can be captured at their displayed bounds. An asynchronous readback superseded by newer photo pixels is retried before a frame event or file is published.
- Keep state checks, pixel checks with declared tolerances, UI review and native checks (dialogs, focus, resize, shutdown in a real desktop session) separate.
- Never report a screenshot as taken when capture is unsupported. A skipped or headless run is not native platform verification.
- Only synthetic fixtures in CI and shared artifacts. Routine logs use fixture identifiers, not private paths or EXIF. Keep personal photos in ignored `fixtures/jpg/` or `private/`.
- Record what was measured: host, build profile, fixture hash, backend, warm or cold cache. Native M4 timings are hardware evidence; VM or Xvfb runs are functional evidence only.

## Agent loop

1. Read the applicable spec and task, including any owner-decision gates.
2. Run `doctor` once in a new checkout or worktree.
3. While implementing, run the tests for the code you are changing and nothing whole-suite or timed, as [when to verify](#when-to-verify) sets out.
4. When the change is complete, run `verify --tier quick` once. For UI or image changes, also run the smoke scenarios the change touches or the `rendered` tier, and inspect the captures as images; for a change to what the GPU draws, run `gpu-qualification` over the stacks it touches.
   On macOS, use the background harness or `develop --background` for every automated GUI launch; use the live API and renderer readbacks to drive and inspect it. Only perform foreground interaction checks when the owner explicitly requests them.
5. For changes under `crates/`, answer the [performance rules](performance-rules.md) checklist and, once the feature is complete, run `editor-performance` on a generated 24 MP input in release, or the `timing` tier.
6. Report exact commands, artifact paths, results and unsupported cases. Update task and feature status only when acceptance is met.

## Packaging

`cargo xtask package` builds an unsigned host development artifact: a ZIP on macOS and Windows or a `.tar.gz` on Linux containing `Luxforge/` with the executable, notices, `build.json` (source revision, dirty state, target, profile, binary and lockfile hashes) and `checksums.txt`. Run smoke against the packaged executable with `--binary`. Packaging is repeatable, not byte-reproducible, and inventory is not a completed license audit. macOS bundles are unsigned and not notarized. No signing, stores or auto-update exist.

## CI

`.github/workflows/check.yml` runs one job per hosted platform, macOS arm64 and Ubuntu x64, each testing, building and packaging the tree with seven-day artifact retention, plus a separate dependency-policy job (`cargo xtask audit`). Windows CI is disabled; Windows support will come later. A hosted lane is compilation and functional evidence of what it ran and what it ran on, never native desktop or GPU acceptance, which only a native machine gives ([pillar 5](../../AGENTS.md#pillars), [GPU-first](../design/gpu-first.md#the-contract)). The release gate, `cargo xtask gpu-qualification`, needs a native GPU and runs on the owner's machines, never on a hosted lane. Each lane records the graphics adapters it had with the packaged editor's `--gpu-adapters` (`artifacts/gpu-adapters*.jsonl`), and every rendered run records the adapter that drew each launch in `result.json` (`launches[].adapter`, with its `device_type`).

| Lane or machine | What runs | What it proves | What it never proves |
| --- | --- | --- | --- |
| Linux CI, no adapter (`ubuntu-24.04`) | `cargo xtask check` (slow tests, doctests and the golden-fixture test included, `luxforge-cli`'s `json_cli` owner journeys among them) and `editor-acceptance` in release, with every Vulkan and EGL driver hidden from the loaders (`VK_DRIVER_FILES`, `VK_ICD_FILENAMES`, `__EGL_VENDOR_LIBRARY_FILENAMES`); then, with the same drivers hidden, `--gpu-adapters` must find no adapter, or the lane fails | The reference renderer's own tests, the catalog owner, the headless `luxforge-json` and the owner journeys, with no graphics adapter at all | Anything of the GPU: its tests skip, saying so, and a skip is not a pass |
| Linux CI, software adapter (the same job, after the build) | Mesa's lavapipe installed; with `WGPU_BACKEND=vulkan`, `--gpu-adapters` must find only `Cpu` adapters; then twelve smoke scenarios against the packaged binary under Xvfb, the original renderer smoke (`empty` through `large60`), `histogram`, `gpu-identity`, `gpu-preview` and `no-gpu-render`, each with `smoke --editor-software-adapter`, so every launch passes `--software-adapter` and draws through the GPU stage on lavapipe, which the editor otherwise refuses until the software adapter is adopted; every launch's recorded adapter must be `Cpu`; the runtime imports are recorded | The rendered journeys, the GPU stage's programs and the GPU-preview path, and the reference renderer as the no-GPU fallback, functionally, on a software Vulkan rasterizer | Native GPU behaviour, performance or tolerance: lavapipe runs on the CPU, so no figure or GPU-against-reference comparison from it is GPU evidence; nor what a software-only host draws by default, which the unit tests and the container run below prove |
| macOS CI (`macos-14`, a hosted VM) | `cargo xtask check` and `editor-acceptance` in release, the optimized build and packaging; `--gpu-adapters` records whatever adapter the VM exposes | The reference renderer's tests, the owner thread and the build on macOS arm64 | Any GPU result: macOS has no software Metal, a VM's paravirtual Metal device is not native, and no rendered scenario runs |
| Windows CI | Disabled | Nothing | Anything on Windows |
| The owner's M4 Pro, native | Every tier on Metal, the rendered scenarios and the GPU tests on its own adapter, and timing on a quiet host | Native GPU evidence on Apple silicon with its driver, which each run's recorded adapter names | Windows or Linux GPU behaviour |
| An arm64 Linux container on the M4 (Docker Desktop, Mesa lavapipe, no GPU passed through) | The drag-tick benchmark on lavapipe, its own release build in the container ([software adapters](../specs/performance.md#software-adapters)); and, under Xvfb, the `load` scenario as a launch starts by default and `gpu-preview`, `gpu-identity`, `histogram` and `no-gpu-render` with `--editor-software-adapter`, as the CI lane runs them (passed on 2026-10-06, the editor built at `885e741e` and the harness with the software wording of the smoke checks) | What the GPU path costs on a software rasterizer running on the M4's cores, against the reference renderer's frame on the same cores: the measurement the software adapter's adoption is decided on. That a software-only host draws the reference by default (`launch_renderer` `software-adapter-not-adopted`, the status bar's "Reference renderer") and the GPU stage on lavapipe when asked (the session's GPU record with `software: true`, "Software GPU preview"), functionally | A typical PC's figures (the M4's cores are faster than most), Windows' WARP, or any GPU evidence |
| A Windows or Linux machine with a GPU, native, when one is available | The rendered scenarios and the GPU tests on its adapter | Native GPU evidence for that adapter and driver, recorded with the hardware | Any other machine |

The scenario list is written out in the workflow because `smoke --list` says which scenarios need a supplied RAW but not which run on software Vulkan; the other scenarios are not run in CI. Locally, `cargo xtask doctor` lists the adapters a built release editor sees, through the same `--gpu-adapters`. Inspect actual run results for the tested commit; a configured step is not a passing result. Fresh hosted verification of the current tree and manual Windows/Linux desktop checks are open items on the [roadmap](../plan.md).

### Lens edge annotations

`lens-qualification` reads the existing RAW manifest reader and an annotation file with `{"format":1,"sources":[{"id":"manifest-id","edges":[[[x,y],...],...]}]}`. Coordinates are full-resolution content pixel centres before optional recipe geometry. Each marked photo needs at least three edges with at least five points each. A corrected edge must have at most 25% of the original maximum orthogonal line-fit deviation and at most 3 px after normalization to a 6,048-px long side. An empty `sources` array exercises selection and export for every manifest photo while reporting edge quality untested. It does not qualify missing photographs or fabricate edges. Outputs include the query, frozen selection, mapping descriptor, export result and unchanged source hash.
