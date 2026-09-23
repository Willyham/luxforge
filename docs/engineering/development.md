# Development, verification and packaging

All tooling is Rust: `cargo xtask <command>`. Commands reject unknown arguments and pass paths to child processes without shell interpolation. `cargo xtask help` lists everything.

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
| Environment report | `cargo xtask doctor` |
| Full local and CI checks: repository links and task plans, formatting, Clippy, tests | `cargo xtask check` |
| A whole verification tier with one summary | `cargo run --release --locked --package xtask -- verify --tier quick\|rendered\|timing\|full --output NEW_DIR [--jobs N] [--binary PATH] [--manifest FILE]` |
| Individual steps | `cargo xtask check-repository`, `fmt`, `lint`, `test`, `build [--release]` |
| Run the editor, release build | `cargo xtask develop [--catalog FILE] [--open PATH] [--data-root DIR]` |
| Run an unoptimized build, debugging only | `cargo xtask develop --debug ...` |
| Run an agent's editor check without taking focus (macOS) | `cargo xtask develop --background --catalog FILE [--open PATH]` |
| Exact current-editor journey, display-independent, including the Basic and histogram chapter | `cargo run --release --locked --package xtask -- editor-acceptance --output NEW_DIR` |
| Core timing on a real-sized JPEG | `cargo run --release --locked --package xtask -- editor-performance --source JPEG --output NEW_DIR [--samples N]` |
| Desktop slider/curve-to-presented-frame and settled-histogram timing, peak RSS, scratch and idle CPU; `--action`/`--parameter` measure any other slider that drafts — a field-patch slider (presence, mixer, vignette, ...) or a RAW slider, whose action declares that one parameter, over a RAW `--source` — in place of the default Basic exposure | `cargo run --release --locked --package xtask -- editor-latency --source JPEG\|RAW --output NEW_DIR [--binary PATH] [--samples N] [--mode drag\|commit\|burst] [--control slider\|curve] [--action ID --parameter NAME] [--crop DEGREES] [--basic] [--idle]` |
| Verify golden fixtures; generate 24 MP, 60 MP and the mixer and presence scenarios' own hue-wheel and gradient/edge/texture/flat workloads | `cargo xtask fixtures`, `cargo xtask generate-fixtures [--output NEW_DIR]` |
| RAW corpus integrity; independent numerical stage references | `cargo xtask raw-corpus --manifest FILE --output NEW_DIR`, `cargo xtask raw-reference --output NEW_DIR` |
| Authentic RAW editor journey, reopen and resource sampling; `--samples` defaults to 3 trials per source | `cargo run --release --locked --package xtask -- raw-editor --manifest FILE --output NEW_DIR [--samples N] [--binary PATH]` |
| Rendered smoke scenario, needs a native graphical session | `cargo xtask smoke --scenario NAME --output NEW_DIR [--binary PATH]` |
| Rendered crop workflow and overlay | `cargo xtask smoke --scenario crop --output NEW_DIR`, `--scenario crop-draft` |
| Rendered workspace panels, mode, preview, the Transforms icon row, conflict and palette; unavailable-provider notice | `cargo xtask smoke --scenario workspace --output NEW_DIR`, `--scenario unavailable` |
| Rendered Basic slider gesture: draft, commit, typed value, undo, reset and conflict | `cargo xtask smoke --scenario basic --output NEW_DIR` |
| Rendered Basic panel: all three groups, historical values, a group reset, the neutral picker, and the default screen with Basic expanded and every other section collapsed | `cargo xtask smoke --scenario basic-panel --output NEW_DIR` |
| Rendered histogram, clipping overlays, pointer readout and a drafted frame | `cargo xtask smoke --scenario histogram --output NEW_DIR` |
| Rendered Basic composed with crop and straighten | `cargo xtask smoke --scenario basic-crop --output NEW_DIR` |
| Rendered restart: a Basic edit committed in one launch and reopened in the next | `cargo xtask smoke --scenario basic-restart --output NEW_DIR` |
| Rendered Presence: section expand, a Clarity drag and cancel, Texture and Clarity each committed at Fit and 100%, Dehaze at both signs, all three fields at once through the raw API and the module reset, over a generated gradient/edge/texture/flat fixture | `cargo xtask smoke --scenario presence --output NEW_DIR` |
| Rendered Colour mixer: section expand, a Red hue drag and commit at Fit and 100%, a Saturation group reset, a stronger hue shift and the Saturation and Luminance tabs, over a generated hue wheel | `cargo xtask smoke --scenario mixer --output NEW_DIR` |
| Rendered Vignette: section expand, an Amount drag and commit at Fit and 100%, roundness and feather extremes, a post-crop recentre and the module reset | `cargo xtask smoke --scenario vignette --output NEW_DIR` |
| Rendered percentage zooms: 50%, 100%, 120%, 800% and 1600%, pans to the centre and the far corner at 1600%, and idle checks at Fit, 100% and 1600%, over the generated 24 MP and 60 MP JPEGs, one launch each | `cargo xtask smoke --scenario zoom --output NEW_DIR` |
| Rendered Presets: section expand, an XMP and a Lightwell preset imported, each applied from its row, undo, the create form filled and submitted, a native preset applied to the Original, `preset.list` through the `api` step and a delete through the row menu | `cargo xtask smoke --scenario presets --output NEW_DIR` |
| Rendered Performance section: open and sampling from the launch, a filled window, a straighten and a Presence Clarity commit whose render is listed as long work and then as finished, collapsed and asleep, then reopened on a fresh window, over the generated 60 MP JPEG, with the editor's memory read by the runner from outside the process; `--source RAW` swaps the Clarity commit for a RAW temperature commit, outside `rendered` | `cargo xtask smoke --scenario performance --output NEW_DIR [--source RAW]` |
| Rendered RAW section over a supplied RAW file, with Basic collapsed; a Custom temperature drag left open and then released, at Fit and at 100%, whose drafted frame differs from the one before, is labelled approximate and adopts no histogram, and whose release's exact frame is the first drawn after the commit, carries its own report and is within a code of the approximate one on average, each drag keeping the tint in force (the first, from As shot, the core's as-shot tint) in the committed payload and in the Custom tint field throughout; then a double-click on each RAW slider and on Basic's Exposure: the first press's committed jump and the reset that follows it, each checked as two entries with the reset sent against the jump's revision and never refused — Exposure back to 0 EV, Custom temperature and tint back to As shot (`use-as-shot-wb`, the entry labelled As shot white balance, both fields showing the core's as-shot equivalent, checked back through the forward map); and the RAW band's dot, absent on the untouched photograph, present after the committed custom temperature and absent again once the resets leave As shot at 0 EV; then a crop drafted on the RAW's whole input stage, 16:9 and straightened by 7°, whose draft is one picture (under 1% of up to 4800 samples of its interior show the canvas), applied at Fit, read at 100% through two pointer readouts and replaced by a −12° 3:2 `edit.crop-fit` at 100%: no step logs a failure, every committed frame shows the current entry at the output its payload declares, placed and centred at Fit within 4 px, and each readout's codes are the canvas's own at that stage pixel within one code; not in `rendered`, because no RAW photograph is checked in | `cargo xtask smoke --scenario raw-panel --source RAW --output NEW_DIR` |
| Rendered module capabilities: settings, a profile and a masked key, the download consent denied then allowed, install, activation, the photo-data consent, a task with progress, Apply, a refused task and a revoked grant, against a loopback proof endpoint | `cargo xtask smoke --scenario capabilities --output NEW_DIR` |
| The capability framework's own costs (registration, capability reads, activation, a task, artifact publish, cancellation), release only | `cargo test --release --locked -p lightwell-core --lib capability_timing -- --ignored --nocapture` |
| Inspect a capture | `cargo xtask check-capture --image PNG [--orientation N]` |
| Process failure checks; macOS measurement, `--samples` defaults to 5 launches per workload | `cargo xtask hardening --binary PATH --output NEW_DIR`, `cargo xtask measure --binary PATH --output NEW_DIR [--samples N]` |
| Package; dependency inventory | `cargo xtask package --output NEW_DIR`, `cargo xtask inventory --output NEW_DIR` |
| License, source and advisory policy | `cargo xtask audit`, see [dependencies](dependencies.md) |
| Isolated UI probes | `cargo xtask probe --candidate iced|egui --output NEW_DIR` |

Every evidence command refuses an existing output directory: use a fresh `artifacts/<run-id>/`. Default sample counts are functional runs: they prove the journey and give one launch count to quote, not a distribution. A p50/p95 claim needs the explicit counts stated in the [performance plan](../specs/performance.md#sample-counts-for-a-p50p95-claim). Timing commands must use release builds. A debug build makes image work roughly thirty times slower (a 10 MB JPEG took ten seconds to open), which is why `develop` defaults to release. `check` never implies graphical or dependency-audit acceptance.

### Verification tiers

`verify` runs a tier of the commands above and writes one summary. Each tier includes the ones below it:

| Tier | What it runs |
| --- | --- |
| `quick` | `check` and `editor-acceptance` |
| `rendered` | quick plus all 26 smoke scenarios, including `zoom`, `presets`, `gallery`, `controls`, `capabilities` and `performance`, through a bounded pool |
| `timing` | quick plus `editor-performance`, `editor-latency` and `measure`, in that order, serially, after everything else in the tier and behind the host-wide timing lock |
| `full` | rendered plus timing plus `raw-reference` and, with `--manifest FILE`, `raw-editor` |

The local default is `quick` per change; run `rendered` and `timing` at integration points and `full`
before a milestone claim. Without a manifest, `full` lists `raw-editor` as `skipped` with the reason
`no --manifest`: a skip is never a pass.

The command builds `lightwell-app` and `xtask` once in release, then runs each component as a child
process of the release `xtask` executable with its console output in `<out>/<component>/console.log`
and its own evidence in `<out>/<component>/run/`. `--binary PATH` is forwarded to every component
that takes one; without it the executable just built is passed explicitly, so every component
measures the same file. The rendered and timing tiers run `generate-fixtures` first when the 24 MP,
60 MP or hue-wheel workloads are missing. A component that has stopped making progress is killed after twenty
minutes and recorded as `timed_out`.

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
first one starts, `verify` takes a host-wide lock — `lightwell-timing.lock` in the OS temporary
directory, holding the pid, treated as stale only once that pid is no longer alive — and releases it
at the end of the run, including on failure. `measure`, `editor-latency`, `editor-performance` and
`raw-editor` take the same lock when run by hand, so an ad hoc timing run and a `verify` timing tier
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
and the summary header names the component. Load never fails the run by itself.

The console shows only the Markdown table. `<out>/summary.json` and `<out>/summary.md` hold, per
component in plan order, its status, start offset, elapsed time, exit code, first failure line,
artifact paths and how many editor processes it started, then the p50/p95 timing rows of the timing
tier with their source file, sample count, load and reliability, and each provisional performance
target with its measured figure and a `pass`, `miss`, `unreliable` or `not_measured` verdict. Both
files are rewritten after every component, from whichever worker finished it, so a partial run still
reports what it has; components that never ran are `not_run`. The exit status is non-zero when any
component failed or timed out, and names them.

The command never opens a frame. Read a capture as an image only for a failed scenario or a design
review.

Wall-clock on the owner's M4 Pro, release build already current and the Cargo cache warm, on a host
shared with other work at one-minute load averages between 4 and 13: `quick` 9 s, of which `check`
is 8 s and varies with how much Cargo has to redo; `rendered` 18 s, a measured 17-scenario workload with 19 editor
launches taking 9 s of wall clock through the pool against 26 s of their own summed elapsed time, or
23 s serially with `--jobs 1`; `timing` 70 s with the default sample counts, of which `measure` is
48 s and 17 launches, `editor-performance` 4 s and `editor-latency` 5 s; `full` with the owner's three-source
manifest adds `raw-reference` and `raw-editor`, whose default three trials per source cost about 8 s
each for the Z6, 19 s for the X100VI and 18 s for the Air 2S, two launches per trial. On a run whose
`measure` started at a load average of 8.33, that component's rows and target verdicts came back
`unreliable` while the two components below the threshold still gave verdicts.

### Rendered scenario cost: why every scenario stays in `rendered`

Per-scenario elapsed time comes from each run's own `summary.json`. Back to back on the same shared
host, one-minute load averages of 12.96 (`--jobs 1`) and 13.24 (the default pool of three), a 17-scenario workload (excluding the gallery and controls boards) cost 24.40 s serially and 30.00 s summed inside the pool, whose own wall clock was 10.30 s.
Every scenario costs one to two seconds either way, and none exceeds 2.3 s; against a rendered tier
whose own total stays under two minutes, no single scenario is a material share of it. Every scenario
therefore stays in `rendered`; `full` adds only the RAW components (`raw-reference` and, with
`--manifest`, `raw-editor`), which is already the tier's composition. `zoom`, which is not in that
workload, is two launches with three one-second idle waits each and took 13.5 s inside the default
pool at a one-minute load average near 20; it stays in `rendered` as the only rendered check of the
percentage zooms. `performance` is one launch with 8.6 s of waits — the sampler needs real seconds
to fill its window and to prove itself asleep — and took 12.5 s on its own at a one-minute load
average near 30; it stays in `rendered` as the only rendered check of the Performance section and of
its sampler's gating.

| Scenario | Serial elapsed (`--jobs 1`) | Pooled elapsed (default `--jobs 3`) | Tier |
| --- | --- | --- | --- |
| `empty` | 1.03 s | 1.8 s | rendered |
| `load` | 0.96 s | 1.2 s | rendered |
| `replacement` | 1.02 s | 2.1 s | rendered |
| `invalid` | 0.91 s | 1.6 s | rendered |
| `repeated` | 1.56 s | 2.0 s | rendered |
| `alternating` | 2.08 s | 2.3 s | rendered |
| `large24` | 1.04 s | 1.2 s | rendered |
| `large60` | 1.17 s | 1.1 s | rendered |
| `crop` | 1.49 s | 1.7 s | rendered |
| `crop-draft` | 1.24 s | 1.3 s | rendered |
| `workspace` | 1.63 s | 1.6 s | rendered |
| `basic` | 1.95 s | 2.3 s | rendered |
| `basic-panel` | 1.70 s | 2.3 s | rendered |
| `basic-crop` | 1.25 s | 1.5 s | rendered |
| `basic-restart` | 1.87 s | 1.9 s | rendered |
| `histogram` | 1.69 s | 2.0 s | rendered |
| `unavailable` | 1.80 s | 2.3 s | rendered |

Reproduce with `verify --tier rendered --output NEW_DIR` for the pool and `--jobs 1` for the serial
figures. The summary records the load average for timing components only; the loads quoted above
were read with `sysctl -n vm.loadavg` immediately before each run.

## Running the application

`cargo xtask develop` starts the editor. It owns the catalog (`--catalog FILE`, defaulting to the platform configuration directory), offers native Open with Cmd+O or Ctrl+O and starts an authenticated loopback JSON service. `--data-root DIR` isolates config, cache and log paths. The application also accepts `--window-size W H` (320 to 4096 logical), `--developer`, `--disable-module MODULE_ID`, `--proof-endpoint URL` (registers the developer capability proof against that endpoint; refused without developer mode), `--hidden-window`, `--evidence-dir NEW_DIR` and `--evidence-script FILE`. `--hidden-window` creates the window invisible: it owns a real surface and renders and captures through it exactly as a visible window does, but the window server never places it on screen. Every automated editor launch the harness makes passes it; `develop` in either mode never does. `--developer` lists proof and diagnostic modules, which are hidden by default so the workspace stays a photo editor; the API is unaffected. `--disable-module MODULE_ID` registers that built-in wrapped as unavailable, keeping its effect identities readable so a stack that uses it reports the unavailable effect instead of rendering without it; an unknown identity is a startup error. Evidence mode is the same editor driven by the harness: each `--open` goes through the ordinary import call into a catalog created inside the new evidence directory, a window frame is captured after each outcome, the script's steps then run with a frame each, and the run exits after writing its results. Manual Open is disabled during collection, and `--open` may repeat only with `--evidence-dir`.

The headless owner reads one JSON request per line:

```sh
target/release/lightwell-json --catalog /path/to/catalog.sqlite < requests.jsonl
```

Start with `schema.list`. Request shapes and live-session behavior are in the [user guide](../user-guide.md). `--data-root DIR` puts module settings, grants and installed resources under that root instead of the platform directories, `--secret-store memory` keeps module secrets for the process only instead of the platform store, `--permission-authority` lets this client grant module consent (an explicit local setup step; without it `module.permission.grant` is `forbidden`), and `--proof-endpoint URL` registers the capability proof module. The desktop's own client always has that authority; an evidence run keeps module state inside its evidence directory with an in-memory secret store, so no automated run touches the person's configuration or login keychain. Only one process owns a catalog at a time; a second instance exits with an explanatory error. Diagnostics go to stderr, or to isolated logs under an explicit data root, never to protocol stdout. Editor mode writes only the catalog and a temporary live-session file beside it; the source original is never written.

On macOS, `develop --background` builds the selected profile and runs a temporary copy in an `LSBackgroundOnly` app bundle, preventing desktop activation. Use an isolated catalog or `--evidence-dir NEW_DIR` for automated checks. The live API and native GPU renderer remain available; this mode is for API and capture work, not keyboard, mouse or native-dialog checks. The bundle is removed after exit, and the original executable and packaged app are untouched. Restricted tool environments must permit macOS LaunchServices/window-server IPC: a background process can otherwise stall before image work, with only startup/open-request events and idle source/catalog workers. Retry with the required host access rather than activating the window. Ordinary `develop` remains an interactive launch. `--background` fails explicitly on other platforms.

### Browsing the component gallery

Debug builds expose the title-bar **Developer** button automatically. To inspect the gallery in
an optimized build, run `cargo xtask develop --developer` (automated launches add `--background`).
Its page chooser and Previous/Next controls browse ten pages; Back to editor or Escape returns.
The `gallery` smoke covers all 78 reference states and the return to the unchanged editor via
the same `workspace.set` path as the button.

## Rendered evidence

Smoke runs the built or packaged editor through a deterministic evidence sequence (repeated `--open`, evidence directory, fixed window size, bounded deadlines); there is no separate viewer, so the captured frame is the editor window with its sidebar. Scenarios: `empty`, `load`, `replacement`, `invalid`, `repeated`, `alternating`, `large24`, `large60`, `zoom`, `crop`, `crop-draft`, `workspace`, `basic`, `basic-panel`, `basic-crop`, `basic-restart`, `histogram`, `presence`, `mixer`, `vignette`, `presets`, `gallery`, `controls`, `capabilities`, `unavailable`; generate the large fixtures first. Each run writes `result.json`, `app/events.jsonl`, `app/state.json`, `app/frame-*.png` (window-renderer readbacks, not OS screenshots), `subprocess.log` and `reproduce.md`. Each frame records `surface_columns`, the physical x range of the photo surface derived from the editor's layout constants, and `canvas_rect`, the canvas region between the panels and the bars as `[left, top, right, bottom]` physical pixels, which at a percentage zoom is exactly the scrollable the photograph pans in; the runner verifies fixture colors, Fit geometry and centering within that range, generation and state, backend, exit status and unchanged source hashes before writing `passed`; blank, stale or missing frames fail. The `render_ready` event marks the upload of the open request's preview raster, which is when a frame becomes capturable. Single-open evidence has a 25-second application deadline; multi-step evidence scripts have 60 seconds for repeated RAW redevelopment. Smoke retains its 35-second process deadline; the RAW editor journey has a 70-second process deadline. These are harness hang bounds, not interactive latency targets.

At Fit, and at any zoom that draws the stage smaller than itself, the frame a scenario captures is
the **display proxy**: the whole recipe rendered against a source downscaled once to the photo area,
which is the size the display was going to minify the exact render down to anyway. `preview_displayed`
therefore carries `proxy`, `proxy_dimensions` (the proxy source's own size, null when there is none),
`proxy_built` (the proxy source was built for this frame rather than taken from the queue's cache)
`reason` (`"zoom"` when the frame is a retained raster a zoom change needed rather than a
render) and `render_ms`: the preview worker's own time for the phase that produced those pixels —
the proxy build when that frame built it plus the render, or the exact render plus the reduction —
excluding the queue wait, source preparation and the hand-over, and for a `"zoom"` frame the time
recorded with that retained raster. It is the figure the status bar states as "Rendered in N ms",
with "(proxy)" for a proxy. Its `dimensions` stay the exact output stage's, which is what picks, the
percent-zoom box and the overlay cell grid map through.

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
nothing, however often the view is rebuilt. It therefore carries no `upload_ms`, and neither does
`render_ready`: there is no upload step to time. The clipping overlay and the crop draft's input
stage keep the toolkit's image widget and still upload, which is what `clipping_overlay` and the
draft's own settle report. `preview_exact_adopted` records the exact phase of such a job
being taken up without an upload, with that phase's own `render_ms`, `preview_exact_cancelled` records one a newer request superseded, under its own generation and with `draft` when it was a crop draft's input stage, and
`clipping_overlay` carries `approximate` while the mask is derived from the proxy on screen rather
than from that exact raster. `preview_failed` records every failed preview of the displayed state,
with its entry and reason, and a scripted step waiting for the newest preview's pixels ends on it
and captures the failure. `preview_withdrawn` records a failed preview whose target was not the
picture on screen: that picture, named with the target, is taken off the surface with everything
derived from it, so `state.surface.raster` and `state.stack.displayed` are null until a frame of the
target renders, and `crop_draft_failed` records a draft whose input stage could not be rendered or was superseded before it rendered (`error_code: cancelled`, with that job's `generation` when it had one). A
crop draft's `state.crop.input_stage_tiles` counts the atlas-sized tiles its input stage was
uploaded as. `state.json` carries `proxy: {eligible, declined, approximate, dimensions, bounds,
presented}`, so a stack that took the exact path — an ineligible layer, a stage already inside the
bounds, a failed build — says so rather than being silently identical, and `status_bar: {readout,
render, render_ms, render_proxy}`, what the bar drew and the figure behind it. The `histogram`,
`basic`, `large24` and `large60` scenarios check that every `preview_displayed` carries a finite
`render_ms` below 5 s and that each captured status bar states one of those figures in the editor's
own wording, with `(proxy)` exactly when the frame on screen is the proxy.

### The Basic and histogram acceptance chapter

`editor-acceptance` ends with a chapter that drives the whole Basic and histogram surface through
the JSON method table with `OwnerHandle::call`, exactly as an independent client reaches it, against
its own catalog inside the run's output directory. Its oracle is the independent f64 reference under
`crates/lightwell-core/tests/reference/`, compiled into `xtask` through a `#[path]` module rather
than copied, so the acceptance journey and the core's own numerical tests check production against
one written-from-the-formulas implementation that production code can never import; the crop
sampler, the exact quarter turn and the histogram reduction the chapter compares against are written
in `xtask/src/basic_acceptance.rs` from the crop spec and the histogram contract, not taken from
`analysis::reduce`. Every result lands in `result.json` under `basic_and_histogram`, and any
mismatch fails the command.

The chapter covers discovery, `edit.set-basic` as a field patch checked whole-raster against the f64
reference, the frozen unit order, `render.sample` against the rendered bytes, the draft lifecycle
with `render.sample {draft_id}` agreeing with the later committed render, a return-to-start no-op,
request deduplication, group and module resets keeping the layer identity, `analysis.request/read`
on current, historical and drafted targets against an independent reduction, cropped-population
semantics, mixed stacks (a straightened 10° crop against a stepwise quantize-then-bilinear
reference, a point replacement before and after the Basic layer, and Basic under an orientation
layer), the ambiguity and unsupported-format refusals, two-client draft conflict and reapply,
analysis sharing and cancellation, an unavailable Basic provider, and a catalog reopen. The
supersede and disconnect races are covered by
`lightwell_core::api::owner::tests::racing_requests_supersede_the_pending_job_and_withdrawal_releases_only_its_own_interest`
and are referenced rather than duplicated.

### Authentic RAW evidence

`raw-editor` runs the actual background editor, then reopens the same isolated catalog in a second process. Each source passes exposure, gain and custom temperature/tint edits, a sensor-neutral pick, geometry, undo, Original/current history selection and Fit/100%. It checks displayed entry/snapshot/layers, bound control values, source hashes, actual photo pixels and exact reopened presentation. These comparisons prove reevaluation and state correlation, not controlled color accuracy. Large RAWs and required DNG corrections can take substantially longer than small fixtures; a script that continues producing correlated frames must be assessed against the whole-journey deadline.

The local manifest has `format:1` and a `sources` array. Each source supplies `id`, `path`, `sha256`, `mode`, `make`, `model`, upright `source_dimensions:[width,height]`, `orientation` and a fixture-verified `neutral_point:[x,y]`. Mode strings, make and model must match the current [camera catalog](../../crates/lightwell-raw/data/cameras.json). DNG sources additionally supply exact `sensor_dimensions`, `active_area` and `default_crop` expectations, and the harness verifies required opcode order, calibration and persisted interpretation against that profile. Keep private paths and derived evidence ignored. `--samples` defaults to 3 (range 1–100), a functional run; a latency distribution needs `--samples 30`. Use an explicit absolute `--binary` and the same `CARGO_TARGET_DIR` for build and harness when working across worktrees.

Reports include binary/lock/manifest hashes, launch mode, stage events, frame checks and sampled process RSS. The filesystem cache is not purged; app-cold is not OS-cache-cold. GPU memory is not isolated from RSS, and capture readbacks can affect memory. Same-process editing without repeated captures is a separate resource control.

Native and JSON authentic-file tests are opt-in, ignored in the normal test suite:

```sh
LIGHTWELL_RAW_OWNER_DIR=/path/to/private/raw LIGHTWELL_RAW_PUBLIC_DIR=/path/to/cc0/raw cargo test --release --locked -p lightwell-raw --test real_files -- --ignored --nocapture
LIGHTWELL_RAW_OWNER_DIR=/path/to/private/raw cargo test --release --locked -p lightwell-app --test raw_json_cli -- --ignored --nocapture
```

The absence of private fixtures is a skip, not passing authentic-file evidence. Synthetic/reference tests remain normal CI checks.

### Evidence scripts

`--evidence-script FILE` takes a JSON array of steps. They run in order after the last `--open`
outcome, each ends in exactly one captured frame numbered after the open frames, and each frame gets
its own `state-<n>.json` and a record in `result.json`'s `script`. A capture reads back the frame
the window renderer drew last rather than drawing a fresh one, so it is taken only once that frame
was built after every update the editor has handled, and the frame's state is recorded at that same
moment: a preview result that arrives beside the capture tick waits for the next frame instead of
leaving the state describing a picture the capture does not show. A step whose frame is due when a
view change asks for a new frame — a zoom that needs a proxy at new bounds, a panel toggle — waits
for that frame, so a capture never shows a proxy made for the previous bounds. Every step goes through the
messages and owner calls the controls use, so a script exercises the real paths rather than a
parallel implementation. Parsing happens before the window opens; at most 64 steps.

```json
[
  {"api": {"method": "edit.crop-fit", "params": {"aspect": "16:9", "angle": 0}}},
  {"draft": {"start": true}},
  {"draft": {"rect": [40, 24, 300, 200]}},
  {"draft": {"preset": "1:1"}},
  {"view": {"zoom": "100"}},
  {"draft": {"apply": true}}
]
```

Each step is an object with exactly one key.

- `api` sends one owner request. For a method the method table lists without a `mutation` envelope,
  such as `preset.list` or `session.state`, the request is sent as written, with the open asset's
  `asset_id` only when the method names one, and its frame is captured when it answers; the
  answer is recorded as the step's `result`, and after a `preset.*` method the library is listed
  again before the frame is captured. For every other method the desktop fills `asset_id` and the `mutation` envelope itself —
  the current state's revision and a fresh request id — and rejects a script that sets either, so any
  asset mutation works, `history.undo` included. Its frame is captured when the resulting preview
  reaches the GPU: the same `render_ready` correlation an `--open` uses.
- `draft` drives the crop draft: `start`, `reapply`, `angle`, `nudge`, `preset` (a declared aspect
  option, by name), `rect` (`[x, y, width, height]` in box pixels, applied as two corner gestures,
  top-left then bottom-right), `swap`, `lock`, `option`, `guide`, `apply`, `cancel`. `start` and
  `reapply` wait for the crop layer's truncated input-stage preview, `apply` waits for its committed
  pixels, and the rest are captured on the next rendered frame.
- `slider` drives one gesture on a generated control: `{"action": "set-basic", "parameter":
  "exposure", "values": [0.25, 0.5, 0.75]}` sends one pointer move per value, exactly as a drag
  produces them. Each move sends its `draft.set` and the one preview job for it as soon as nothing
  is in flight; there is no tick to wait for. `"release": true` ends it
  with the control's release, which commits once; `"cancel": true` ends it with Escape; neither
  leaves the gesture open and captures the frame once the draft has drained, so the pixels belong to
  the newest value it sent. A second `slider` step naming the same control continues the same
  gesture. An `"interval_ms": 8` field paces the values instead of sending them all at once: one
  value is sent per tick of a timer gated on the step still having values left to send, so a wild
  drag can be scripted without the harness deciding what reaches the owner. Each paced value is
  recorded as its own `slider_step_value` event (`{"value", "index"}`), even one the core's own
  gesture round trip coalesces away, so the harness can time an input that never reached the owner.
  Without `interval_ms` every value is sent at once, as before.
- `double_click` double-clicks one drafting slider's rail: `{"action": "set-raw-temperature",
  "parameter": "kelvin", "value": 5000, "gap_ms": 120}`. The first press is the move to `value`,
  which opens the gesture, and its release, which commits it; `gap_ms` (0 to 250) after that
  release, one timer tick sends the reset the rail's wrapper publishes for the second press,
  whatever the commit is doing by then. The frame is captured once nothing the two presses started
  is in flight. `double_click_first`, `double_click_second` and the reset's own
  `field_reset_queued`, `field_reset_sent` or `field_reset_dropped` events record the order, and a
  refused request logs `command_failed`.
- `slider_draft` answers an open gesture's Changed elsewhere notice: `"discard"` or `"reapply"`.
- `field` types into one generated field: `{"action": "set-basic", "parameter": "exposure", "text":
  "1.5"}`, with `"submit": true` for Enter, which commits that one field without a draft.
- `reset` runs a declared reset: `{"module": "lightwell.basic"}` is the module's own header reset and
  `{"module": "lightwell.basic", "group": "Tone"}` is that control group's, found by its label.
- `pick` clicks the photograph at a pixel of the raster on screen: `{"x": 120, "y": 80}`. What the
  click does is the active canvas mode's own declared pick, so a `workspace` step selects the mode
  first; a mode that declares none fails the step. A pick that commits is captured on the render it
  produces, and one that is refused on the status it leaves.
- `view` sets the zoom through `view.set`: `{"zoom": "fit"}` or a percentage from 10 to 1600.
- `pan` scrolls the percent-zoom canvas to a fraction of its scrollable range on each axis:
  `{"x": 0.5, "y": 0.5}` is the centre and `{"x": 1, "y": 1}` the far corner. It goes through the
  same scrollable a Space drag scrolls, and is captured once the offset the scrollable reports has
  reached the session through `view.set`, so `state.surface.view` carries the pan the frame was drawn
  at. At Fit there is no scrollable and the step fails.
- `wait` (`{"ms": N}`, 1 to 10000) asks nothing of the editor for at least that long and then
  captures. The evidence run's own 250 ms tick keeps rebuilding the view meanwhile, as the editor's
  event sync does while a photograph is open, so the frame shows what idling did.
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
- `hover` (`{"x": N, "y": N}`) puts the pointer on one pixel of the displayed raster, exactly as the
  canvas publishes a move, and is captured once `render.sample` has answered with the three output
  codes under it.
- `preset` clicks one row of the Presets section: `{"name": "Soft film", "group": "Synthetic"}`.
  The name matches exactly, case included, and `group` is needed only when two groups hold that
  name; no match, or more than one, fails the step. The click is the section action's own
  `RunAction`, so the frame is captured on its pixels like an `api` step's.
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
- `performance` opens or closes the state panel's Performance section through its heading's own
  message: `{"expanded": true}` or `{"expanded": false}`. Opening it, with the state panel shown, is
  captured once the section's first `resources.read` and `activity.list` have answered, so the frame
  shows figures rather than dashes; closing it, opening it under a hidden panel and asking for the
  state it is already in are captured on the next frame. The section is open at every launch, so
  every scripted run samples once a second unless its script closes the section.

- `capability` drives one gesture on a module's capability block, task control or consent notice through the messages those controls send: `{"module", <one of>, "wait"?: false}` with `section` (`"status"` or `"settings"`), `set` (`{field, value, profile?}`), `secret` (`{field, value, profile?}`, recorded in `result.json` as `<redacted>`), `file` (`{field, path}`, the message the native dialog's result sends), `profile` (`{create: {adapter, label}}` or `{remove: index}`), `install` or `remove` (`{resource}`), `activate` (`true` or `false`), `task` (`{task}`), `consent` (`"allow"` or `"deny"`), `apply`, `cancel` (the newest live job), `revoke` (an index into the permissions list) or `settle`. A step is captured once its round trips have answered and the jobs it started have finished; `"wait": false` captures as soon as a started job reports progress, and a later `settle` captures once the module's jobs are done. `state.json` carries a `capabilities` summary per module — settings with secrets only as `set` or `not set`, profiles, activation, resources, live jobs, the task result, permissions, requirements and the open consent — and stack layers carry their `artifacts`.

A step that cannot be sent is recorded with `"status": "failed"` and its reason and still captures a
frame, so a refused step is visible in the evidence instead of missing from it.

The `crop` and `crop-draft` scenarios use this. `crop` commits a 16:9 `edit.crop-fit` and an
off-centre 7° `edit.crop`, then drafts on that layer, straightens to 12°, cancels, drafts again,
nudges and applies. `crop-draft` opens a neutral draft and exercises corner gestures, a declared ratio
preset, a drag on the angle's rail to 2.4° (checked in state: the angle, the ratio kept, the
release's one logged change and no commit), 100% and Fit, then applies the straightened square. The runner checks the committed stack in each frame's state (one
crop layer keeping its identity, the payload that was sent, the revision each commit produced), that
the displayed image has the ratio the committed payload declares, and, on draft frames, that the
rectangle drawn at full opacity matches the captured draft rectangle, that all eight handles are
present and that the stage outside the rectangle is dimmed toward the window background. Each run
also writes `app/crop-checks.json` with the measured values and their tolerances.

`basic-panel` opens `fixtures/s0/greyscale.jpg` at 1440 × 900 and drives a Temperature drag, a typed
Vibrance, a preview of the Temperature entry and its return, the Colour group's reset, the neutral
picker's mode, a pick on a neutral grey patch and a pick on a clipped one. Its opened frame is also
the default screen the Module panels density is accepted on, with Basic expanded and every other
section collapsed by its own descriptor. That fixture is used
because the picker needs both a genuinely neutral patch and a clipped one, and it has each: uniform
grey quadrants and a white cross at code 255. The runner checks, per frame, the revision, the history
label, the stored Basic payload, the one Basic layer's identity across every edit and what each of
the ten generated fields showed; that the previewed frame reports "Previewing entry 1" and shows that
entry's own values rather than the current ones; that the picker frame reports the Basic module as
the workspace mode; that the neutral pick committed temperature and tint of 0 once and kept the mode;
and that the clipped pick's status leads with `clipped:` and committed nothing. Alongside the state
it reads the photograph's mean red-minus-blue balance over a centred window — the measure a white
balance moves on a neutral fixture, where luminance barely changes — and its placement and 3:2 aspect
from the bright pixels of the photo surface, since a greyscale fixture has no quadrant colours to
match. Each run also writes `app/basic-panel-checks.json` with the measured values and tolerances.

`presets` opens `fixtures/s0/orientation-1.jpg` at 1440 × 900 and drives the Presets section over
thirteen steps: Basic collapsed and Presets expanded, `fixtures/presets/develop.xmp` and
`fixtures/presets/soft-film.lwpreset` imported (their names differ only in case), each applied from
its row, `history.undo`, the create form filled with the Basic Tone group alone and then submitted,
`history.undo` to the Original, the native preset applied there, `preset.list` through the `api` step
and the native preset deleted through its row menu. The runner checks every frame's `state.presets`
rows (name, group, Partial) against the expected library, the XMP's import status line, the create
form's fields and checkboxes, and each frame's revision, current entry, history label and stored
layer payloads. The expected payloads are computed stepwise: each fixture's settings as
`lightwell_core::inspect_preset` reads them, merged over the stack the frame before held, with every
field at its declared default omitted as the modules store it. It also reads one patch per quadrant
of the photograph: each +0.35 EV preset brightens the four patches' mean luminance by more than 5
codes, the XMP's own `green-luminance` and `red-hue` fields darken the green patch and add green to
the red one by more than 5 codes, and each undo returns the patches of the stack it returns to within
1.5 codes. It writes `app/presets-checks.json`. The scope is stored payloads and displayed direction,
not a colorimetric claim, and not a claim that Lightwell renders what Lightroom renders.

`performance` opens the generated `60mp.jpg` at 1440 × 900 and captures eight frames: the open, with
the Performance section open and sampling as every launch starts it; a 3600 ms `wait`; a 16:9
`edit.crop-fit` at 3°; `edit.set-presence` with Clarity 100 over it, whose exact render — about
0.8 to 1 s on the owner's M4, where Clarity alone is about 0.6 s — is long enough for the section to
list; a 2500 ms `wait`; the section collapsed; a 2500 ms `wait`; and the section opened again. Each frame's
`state.performance` records the flag, the reads asked for, the samples held, the last two
`resources.read` answers as the owner sent them with the wall-clock time of the newer one, the last
`activity.list`, the process id, the heading caption and the rows and job rows as shown. The runner
re-derives, without the editor's code, the memory figure from the recorded `memory.bytes`, the CPU
and GPU figures from the rate between the two recorded reads, each series' length from the sample
count, and the job rows, `+N more` and caption from the recorded `activity.list` under the section's
display rules; it requires at least four samples after the first wait, the finished render listed
after the second, `footprint` memory with GPU time and unified GPU allocations on the M4, GPU time
never decreasing across frames, one revision per edit and nothing else, the collapsed frames'
reads and samples unchanged across their wait, and the reopened frame holding one sample from one
more read, a fresh window read at once. While the editor runs, the runner reads the same
pid with `ps -o rss=` every 100 ms and `footprint -f bytes --noCategories` on every other poll,
which needs no privileges for the same user; each expanded frame after the open (whose first reads can precede the runner's first reading) must have its recorded resident memory and footprint
lie between the runner's last reading at or before the sample's wall-clock time and its first
reading after it, within 8 MiB. While the editor idles the three agree to the byte. The readings are
in `process-readings.json` beside `app/`, and `app/performance-checks.json` records every comparison
and tolerance. With `--source RAW` the Clarity commit becomes a RAW temperature commit, which
redevelops the mosaic, and the finished job is whichever of the redevelopment and its render ended
last; that run is not part of `rendered`.

`zoom` is two launches, one over each of the generated `24mp.jpg` (6000 × 4000) and `60mp.jpg`
(10000 × 6000), each writing its own `24mp/` or `60mp/` directory beside the scenario's `result.json`.
Each opens the photograph at 1440 × 900 and runs thirteen frames: the open, a 500 ms `wait` that lets
the refit to the display scale land, a 1000 ms `wait` at Fit, 50%, 100%, a 1000 ms `wait` at 100%,
120%, 800%, 1600%, a `pan` to the centre, a 1000 ms `wait` there, a `pan` to the far corner and Fit
again. Per frame the runner checks the session's zoom; that the surface holds the proxy — the stage
scaled into the bounds the view asks for now — wherever the stage is drawn smaller than itself and
the exact stage from 100% up, both in `state.proxy` and `state.surface` and in the version-th
`preview_displayed`; and, at every percentage, that every sample on a 12 px grid over the canvas
that lands at least 12 source pixels from any drawn feature shows the quadrant colour the zoom and
the pan put under it, within 10 codes. Where the white centre line or the middle boundary is in
view, its measured position must be the one the geometry predicts, within one source pixel plus
3 physical pixels; at 1600% on the 60 MP fixture the centre line is exactly where its two textures
meet. Across frames, an unchanged raster version must mean an unchanged texture write count. Each
`wait` frame must show no `preview_displayed`, `preview_proxy_requested` or `render_ready` since the
frame before, the same version and write count, at least two view rebuilds, and a canvas identical
byte for byte to the frame before. A `Validation Error`, `wgpu error` or panic in the editor's log
fails the launch. Each launch writes `app/zoom-checks.json` with the measured values and
tolerances.

`workspace` opens `fixtures/s0/orientation-1.jpg` at 1440 × 900 and drives a rotate, three panel and
thirds changes, a historical preview and its return, a crop draft, a commit during that draft (the
conflict, since any other commit while a draft is open marks it conflicted, whoever made it) and the
command palette, before cancelling the draft. The runner checks, per frame, that `state.workspace`
matches the requested panels, mode and thirds and that the photograph stays centred in the recorded
`surface_columns`; that the historical-preview frame's `state.status` starts with "Previewing entry
0"; that the conflict frame's `state.notices` names "Changed elsewhere" and `state.crop.conflicted`
is set; that the draft frames report the crop module as the workspace mode and the cancelled frame
reports `pointer`; that the palette frame's `state.palette` records it open with its query; and that
the thirds frame's fitted photograph reads brighter at its one-third column than beside it. It writes
`app/workspace-checks.json`.

`basic` opens the same fixture at 1440 × 900 and drives the whole Exposure gesture: a drag to
+1.00 EV left open, the same gesture released, a second drag that returns to +1.00 and releases, a
typed −0.50 with Enter, `history.undo`, the Tone group's reset, a drag to +2.00 EV, a commit by
another route while that drag is open, and the notice's Reapply and Discard in turn. The runner
checks, per frame, `state.draft` (its identity, the field it holds, both revisions and whether it is
conflicted), the revision, the current entry's stored label, what the Exposure field shows, the one
Basic layer's stored payload, and the photograph's own mean Rec. 709 luminance over a centred window
of the photo surface: +1.00 EV and +2.00 EV read brighter than neutral by at least 10 codes, −0.50 EV
reads darker, the committed render matches the drafted one it replaced within 2 codes, and the
reset and discarded frames match the committed stack within the same tolerance. It writes
`app/basic-checks.json` with every measured mean and both tolerances. The scope is displayed
brightness read back from the renderer, not a colorimetric claim.

`histogram` opens the same fixture at 1440 × 900 and drives the inspector, both clipping overlays and
the pointer readout over twelve frames: the default screen, one `edit.set-pixel` of `(0, 128, 255)`
at content pixel 360, 240, a hover over that pixel, the shadow overlay alone, both overlays, 100%,
Fit again, both overlays off, a preview of the Original, the return to current, an Exposure drag
left open and the same gesture released. The fixture's clipped pixels are known from
its generator rather than guessed — the quadrant colours reach neither endpoint, the white centre
line and arrow are at code 255 in every channel and the dash band across the middle is at code 0 in
every channel — and the set pixel is the only one in the run with a channel at each endpoint, which
is both the magenta case and the isolated-clipped-pixel case a Fit overlay must survive. The runner
checks every frame's eleven counters, the plot's shared maximum and the counts the triangles'
tooltips state in words against `analysis::reduce_raster` of an **independent** core render of the
same fixture through the same recipe, exactly, and that the plot's tooltip names the domain and a
frame with a report draws no notice over the plot; that the readout reports the codes of the pixel
just set, is shown in the status bar and clears when the displayed entry changes; that across the
hover the tools panel is pixel for pixel the frame before it and the status bar changed only inside
one readout-slot-wide span short of its trailing facts, so the readout moved nothing; that the
overlay's cell grid is one cell per source pixel at Fit and at 100%, where the photograph also
measures 480 physical pixels wide; and, by differencing each overlay frame against the overlay-off
frame of the same stack and zoom, that blue appears over the code-0 dashes, red over the code-255
line, magenta on the one both-endpoint pixel, nothing over the unclipped quadrant interiors, and
nothing at all once both flags are off. Differencing rather than classifying a colour is deliberate:
the fixture's own red quadrant is as red as a highlight mask is, so only the change from the same
frame without the mask identifies one. Each overlay frame's `state.stack` is compared with the frame
before it, which is how the run proves a view flag commits nothing. Its last three frames return to
current, drive an Exposure drag left open and then release it: the drafted frame must display the
draft revision it names in `state.draft`, and its plot must name that revision and carry counts equal
to an independent reduction of the drafted stack; the released frame must advance the revision by
exactly one and carry counts equal to an independent reduction of the composed stack it says it
displays. It writes `app/histogram-checks.json`. `unavailable`'s second launch checks that the
histogram is unavailable and that its reason is the notice drawn inside the plot.

`basic-crop` opens the same fixture at 1440 × 900 and commits `edit.set-basic` at +1.00 EV, then a
16:9 `edit.crop-fit` at angle zero and the same ratio straightened by 7°. Every one of its four
frames is checked against an independent core render and reduction of exactly the layers
`state.stack.displayed` names, so the plot is proved against the composition of colour and geometry
rather than against itself; the run also checks that the two crops update one crop layer in place,
that the analysed output stage shrinks with the crop, and that the displayed photograph measures
16:9 and stays centred in the photo surface. Its placement measurement finds the photograph by
brightness rather than by the fixture's quadrant colours, because a Basic edit moves those colours.
It writes `app/basic-crop-checks.json`.

`basic-restart` is two launches, since a restart cannot be simulated inside one process. The first
opens `fixtures/s0/orientation-1.jpg` and commits one `edit.set-basic` patch of exposure +1.5 EV and
temperature +25. The second reuses that launch's own catalog (`--catalog <dir1>/catalog.sqlite`) and
reopens the same file, which the catalog dedupes to the same asset. The runner checks that the
second launch reports the same revision, entry, stored payload, Basic layer identity and history
label, that the generated fields re-seed to `1.5` and `25`, and that the photograph's mean Rec. 709
luminance matches the render the first launch committed and is above a neutral open. It writes
`basic-restart-checks.json` beside its own two launch directories.

`unavailable` is not one launch but two, since a module can only be disabled at startup. The first
opens the fixture and commits a 16:9 `edit.crop-fit` with every built-in module registered. The
second reuses the first launch's own catalog (`--catalog <dir1>/catalog.sqlite`) with
`--disable-module lightwell.crop` and reopens the same fixture, which the catalog dedupes to the same
asset by file identity, so its stack still names the now-unavailable crop layer. The runner checks
that the second launch's frame reports `state.render_error.code` `incompatible`, `state.notices`
naming "Preview is stale", the crop module listed unavailable in `state.modules`, no fixture colour
drawn anywhere in the photo surface, and the source fixture's hash unchanged throughout. It writes
`unavailable-checks.json` beside its own two launch directories rather than one `app/` directory.

`cargo xtask smoke --scenario gallery --output NEW_DIR` captures all 78 named widget states across ten pages in the real background editor at 1440×1000 logical points. Each page has renderer readback, state metadata and a matching script event; the board includes every named vector icon at 12 and 16 points and, on its last page, module sections at the reference panel width: Basic expanded, collapsed and unavailable bands, Basic with its three groups collapsed, the tab row, the labelled buttons, band hints and history labels truncated to one line with an ellipsis, the icon-button row and the field rows each with no group header over its module's only group, and the crop section drafting and idle. `cargo xtask smoke --scenario controls --output NEW_DIR` enables the developer proof, scrolls its generated panel, and exercises slider/picker/curve drafts, cancellation, channel selection, point add/remove, discrete controls, group disclosure (on Basic's Colour group, since the proof's controls are its module's only group and draw no header) and the module reset, then shows the Pixel section on its own with X and Y as px fields. Its checks correlate history revisions and values with captures and verify that the identity proof preserves the displayed photograph. The gallery and generated panel have different widths; both require visual review alongside their automated checks.

`cargo xtask smoke --scenario capabilities --output NEW_DIR` starts the loopback `ProofEndpoint` in the runner's own process with a sentinel API key and a held palette download and generation, launches the editor with `--developer --proof-endpoint`, opens `fixtures/s0/orientation-1.jpg` at 1440 × 900 and scripts 25 capability steps: expand the section, set strength, create a profile, set its endpoint and key, choose an input file, request the palette (the consent notice, Don't allow, the notice again with its denial, Allow), capture the download in progress and installed, activate, generate (the photo-data consent, Allow, progress, success), Apply, replace the key with a wrong one and generate again (the endpoint's 401), and revoke the photo-data grant. Its checks, written to `app/capabilities-checks.json`, compare each frame's capability summary with its step; require exactly one tint layer listing the task's artifact after Apply; compare the tinted photograph's mean colour over a centred window with the pre-Apply frame, in the direction of the published gains, and with an independent core render of the same stack within 2 codes; confirm the endpoint saw one held download, one authorised generation and one refusal; and scan every text file the run wrote, the catalog and the module files included, for the sentinel key.

`editor-latency --control curve` measures the controls proof's middle-point drag with the curve editor visible. The proof's colour stage is identity; this measures the control, query, draft, preview and upload path, not a future Tone Curve image algorithm. Slider remains the default workload. Both use the same provisional 100 ms p95 interaction threshold and retain all samples.

`editor-latency` is the desktop counterpart to `editor-performance`, which measures `render` on the
catalog owner's thread and so cannot see scheduling, GPU upload or presentation. It writes its own
evidence script, drives the release binary through a background evidence launch, and reads the
timings out of that run's `events.jsonl`. Each measured input is one scripted `slider` step left
open, so the step settles only once the gesture has drained: one input, one `draft.set`, one preview
job, one frame, with nothing from the previous input still in flight. `slider_draft_set` gives the
input's time, `slider_draft_preview` names the preview generation that `draft.set` produced, and the
`preview_displayed` of that generation is when that raster became the photo surface's source.
Presented therefore means that update, whose redraw draws the frame, not display scanout: the
figures are an upper bound on the editor's own work and a lower bound on what an eye sees. The
report's `gpu_upload` is null with a note for the same reason `preview_displayed` carries no
`upload_ms`, and `render_and_upload` covers the render and the hand-over together. The measured window is
invisible, so nothing in these runs is composited or scanned out at all; the figures cover the
editor's own path to the texture and say nothing about the cost of putting that texture on a
screen. The last scripted value also
releases, so its drafted preview is superseded by the commit — that is the queue cancellation the
report counts — and it is measured through to the `analysis_adopted` of the committed frame, which
is the settled exact histogram, and to that frame's own first `preview_displayed`
(`commit_to_committed_frame`), which is what a person sees on release. A final burst step sends
every value between two ticks to show the driver's coalescing. A RAW temperature or tint drag is
timed like any other: its drafted values preview approximately on the developed planes, the report
counts those frames by phase in `approximate_white_balance_frames`, and its releases, which wait
for the mosaic to be redeveloped, show in `commit_to_committed_frame`. An input whose preview job
was refused — logged as `slider_draft_unpreviewed`, a RAW draft whose development is not in memory
because a redevelopment is in flight — has no frame of its own, and a drag with one is refused with
that reason rather than timed. A RAW slider's gesture values start from zero when its range holds
it and from its declared default otherwise (Custom temperature's 6504 K); `--mode burst` takes
`--action`/`--parameter` too. `--crop DEGREES` commits a straightening 16:9 crop first, so the measured stack
carries the crop resample as well as the colour pass. `--basic` commits a Basic layer with every
field non-neutral first, so each measured frame runs every one of the module's colour units. `--idle` adds a second workload: one evidence
run commits a Basic layer with all ten fields non-neutral into a catalog that outlives it, then an
ordinary launch reopens the same file from that catalog and is left alone for thirty seconds, which
is where peak RSS with a full stack and idle CPU come from. `latency.json` and `resources.json` keep
every sample, the scratch budget's high-water mark and the correlated state.

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
real-time coalescing of that pace), `presented_frames` and `presented_fps` (every `preview_displayed`
over the run, drafted and committed alike, divided by the seconds from the first `slider_step_value`
to the last of them), `staleness_ms` (each presented drafted frame's own `slider_draft_set` time to
its `preview_displayed` time, paired by generation exactly as drag mode pairs them) and
`frame_gap_ms` plus `max_gap_ms` (the intervals between consecutive presented drafted frames),
`cancelled_exact` (`preview_exact_cancelled` events: full-resolution phases a newer request
superseded, which carry no frame) and `proxy` (the last
presented frame's `proxy`/`proxy_dimensions`).

On macOS, smoke, hardening, measurement, latency, RAW editor and probe subprocesses always use the same background bundle as `develop --background`, and every one of them that launches the editor passes `--hidden-window`, so the run has neither an activated process nor a window on screen. Reports record `launch_mode`; reproduce through the harness to preserve focus protection. A native graphical session is still required. Windows and Linux retain direct launches; background behavior is not claimed there. Measurement launch times include the temporary bundle and executable copy, so they do not measure normal foreground activation, and with an invisible window they do not include the cost of compositing a visible one either.

That an automated launch never takes the desktop is proven once, not per run: `hardening` reads the frontmost application's pid through LaunchServices (`lsappinfo`, no Automation permission needed) while its abrupt-termination child is running and fails if that pid is the editor's, recording it as `frontmost_pid_while_running`. Every other run relies on the bundle and the hidden window and does not re-measure the desktop, so switching applications or locking the screen during a run does not affect it.

Rules for any UI or image check:

- Capture after the intended generation is rendered, tied to state and logs, with explicit provenance. A PNG's existence is not a pass.
- Keep state checks, pixel checks with declared tolerances, UI review and native checks (dialogs, focus, resize, shutdown in a real desktop session) separate.
- Never report a screenshot as taken when capture is unsupported. A skipped or headless run is not native platform verification.
- Only synthetic fixtures in CI and shared artifacts. Routine logs use fixture identifiers, not private paths or EXIF. Keep personal photos in ignored `fixtures/jpg/` or `private/`.
- Record what was measured: host, build profile, fixture hash, backend, warm or cold cache. Native M4 timings are hardware evidence; VM or Xvfb runs are functional evidence only.

## Agent loop

1. Read the applicable spec and task, including any owner-decision gates.
2. Run `doctor`, then pick the [verify tier](#verification-tiers) the change needs: `quick` for any change, including a docs-only one; `rendered` for a change touching rendering or the UI, at an integration point; `timing` alongside it for a change under `crates/` at an integration point; `full` before a milestone claim.
3. For UI or image changes, run a smoke scenario, the `rendered` tier, or the acceptance journey and inspect the capture as an image.
   On macOS, use the background harness or `develop --background` for every automated GUI launch; use the live API and renderer readbacks to drive and inspect it. Only perform foreground interaction checks when the owner explicitly requests them.
4. For changes under `crates/`, answer the [performance rules](performance-rules.md) checklist and run `editor-performance` on a generated 24 MP input in release, or the `timing` tier.
5. Report exact commands, artifact paths, results and unsupported cases. Update task and feature status only when acceptance is met.

## Packaging

`cargo xtask package` builds an unsigned host development artifact: a ZIP on macOS and Windows or a `.tar.gz` on Linux containing `Lightwell/` with the executable, notices, `build.json` (source revision, dirty state, target, profile, binary and lockfile hashes) and `checksums.txt`. Run smoke against the packaged executable with `--binary`. Packaging is repeatable, not byte-reproducible, and inventory is not a completed license audit. macOS bundles are unsigned and not notarized. No signing, stores or auto-update exist.

## CI

`.github/workflows/check.yml` runs `cargo xtask check`, an optimized build and packaging on macOS arm64, Windows x64 and Ubuntu x64 with seven-day artifact retention, plus separate fixture and dependency-policy jobs. Linux additionally runs every smoke scenario against the packaged binary under Xvfb with software Vulkan and records runtime imports. Hosted results are compilation and functional evidence, never native desktop or GPU acceptance. Inspect actual run results for the tested commit; a configured step is not a passing result. Fresh hosted verification of the current tree and manual Windows/Linux desktop checks are open items on the [roadmap](../plan.md).
