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
| Individual steps | `cargo xtask check-repository`, `fmt`, `lint`, `test`, `build [--release]` |
| Run the editor, release build | `cargo xtask develop [--catalog FILE] [--open PATH] [--data-root DIR]` |
| Run an unoptimized build, debugging only | `cargo xtask develop --debug ...` |
| Run an agent's editor check without taking focus (macOS) | `cargo xtask develop --background --catalog FILE [--open PATH]` |
| Exact current-editor journey, display-independent | `cargo run --release --locked --package xtask -- editor-acceptance --output NEW_DIR` |
| Core timing on a real-sized JPEG | `cargo run --release --locked --package xtask -- editor-performance --source JPEG --output NEW_DIR [--samples N]` |
| Verify golden fixtures; generate 24 and 60 MP workloads | `cargo xtask fixtures`, `cargo xtask generate-fixtures [--output NEW_DIR]` |
| Rendered smoke scenario, needs a native graphical session | `cargo xtask smoke --scenario NAME --output NEW_DIR [--binary PATH]` |
| Rendered crop workflow and overlay | `cargo xtask smoke --scenario crop --output NEW_DIR`, `--scenario crop-draft` |
| Rendered workspace panels, mode, preview, conflict and palette; unavailable-provider notice | `cargo xtask smoke --scenario workspace --output NEW_DIR`, `--scenario unavailable` |
| Rendered Basic slider gesture: draft, commit, typed value, undo, reset and conflict | `cargo xtask smoke --scenario basic --output NEW_DIR` |
| Inspect a capture | `cargo xtask check-capture --image PNG [--orientation N]` |
| Process failure checks; macOS measurement | `cargo xtask hardening --binary PATH --output NEW_DIR`, `cargo xtask measure --binary PATH --output NEW_DIR [--samples N]` |
| Package; dependency inventory | `cargo xtask package --output NEW_DIR`, `cargo xtask inventory --output NEW_DIR` |
| License, source and advisory policy | `cargo xtask audit`, see [dependencies](dependencies.md) |
| Isolated UI probes | `cargo xtask probe --candidate iced|egui --output NEW_DIR` |

Every evidence command refuses an existing output directory: use a fresh `artifacts/<run-id>/`. Timing commands must use release builds. A debug build makes image work roughly thirty times slower (a 10 MB JPEG took ten seconds to open), which is why `develop` defaults to release. `check` never implies graphical or dependency-audit acceptance.

## Running the application

`cargo xtask develop` starts the editor. It owns the catalog (`--catalog FILE`, defaulting to the platform configuration directory), offers native Open with Cmd+O or Ctrl+O and starts an authenticated loopback JSON service. `--data-root DIR` isolates config, cache and log paths. The application also accepts `--window-size W H` (320 to 4096 logical), `--developer`, `--disable-module MODULE_ID`, `--evidence-dir NEW_DIR` and `--evidence-script FILE`. `--developer` lists proof and diagnostic modules, which are hidden by default so the workspace stays a photo editor; the API is unaffected. `--disable-module MODULE_ID` registers that built-in wrapped as unavailable, keeping its effect identities readable so a stack that uses it reports the unavailable effect instead of rendering without it; an unknown identity is a startup error. Evidence mode is the same editor driven by the harness: each `--open` goes through the ordinary import call into a catalog created inside the new evidence directory, a window frame is captured after each outcome, the script's steps then run with a frame each, and the run exits after writing its results. Manual Open is disabled during collection, and `--open` may repeat only with `--evidence-dir`.

The headless owner reads one JSON request per line:

```sh
target/release/lightwell-json --catalog /path/to/catalog.sqlite < requests.jsonl
```

Start with `schema.list`. Request shapes and live-session behavior are in the [user guide](../user-guide.md). Only one process owns a catalog at a time; a second instance exits with an explanatory error. Diagnostics go to stderr, or to isolated logs under an explicit data root, never to protocol stdout. Editor mode writes only the catalog and a temporary live-session file beside it; the source JPEG is never written.

On macOS, `develop --background` builds the selected profile and runs a temporary copy in an `LSBackgroundOnly` app bundle, preventing desktop activation. Use an isolated catalog or `--evidence-dir NEW_DIR` for automated checks. The live API and native GPU renderer remain available; this mode is for API and capture work, not keyboard, mouse or native-dialog checks. The bundle is removed after exit, and the original executable and packaged app are untouched. Ordinary `develop` remains an interactive launch. `--background` fails explicitly on other platforms.

## Rendered evidence

Smoke runs the built or packaged editor through a deterministic evidence sequence (repeated `--open`, evidence directory, fixed window size, bounded deadlines); there is no separate viewer, so the captured frame is the editor window with its sidebar. Scenarios: `empty`, `load`, `replacement`, `invalid`, `repeated`, `alternating`, `large24`, `large60`, `crop`, `crop-draft`, `workspace`, `unavailable`; generate the large fixtures first. Each run writes `result.json`, `app/events.jsonl`, `app/state.json`, `app/frame-*.png` (window-renderer readbacks, not OS screenshots), `subprocess.log` and `reproduce.md`. Each frame records `surface_columns`, the physical x range of the photo surface derived from the editor's layout constants, and the runner verifies fixture colors, Fit geometry and centering within that range, generation and state, backend, exit status and unchanged source hashes before writing `passed`; blank, stale or missing frames fail. The `render_ready` event marks the upload of the open request's preview raster, which is when a frame becomes capturable. A 25-second application deadline and a 35-second process deadline bound hangs.

### Evidence scripts

`--evidence-script FILE` takes a JSON array of steps. They run in order after the last `--open`
outcome, each ends in exactly one captured frame numbered after the open frames, and each frame gets
its own `state-<n>.json` and a record in `result.json`'s `script`. Every step goes through the
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

- `api` sends one owner request. The desktop fills `asset_id` and the `mutation` envelope itself —
  the current state's revision and a fresh request id — and rejects a script that sets either, so any
  asset mutation works, `history.undo` included. Its frame is captured when the resulting preview
  reaches the GPU: the same `render_ready` correlation an `--open` uses.
- `draft` drives the crop draft: `start`, `reapply`, `angle`, `nudge`, `preset` (a declared aspect
  option, by name), `rect` (`[x, y, width, height]` in box pixels, applied as two corner gestures,
  top-left then bottom-right), `swap`, `lock`, `option`, `guide`, `apply`, `cancel`. `start` and
  `reapply` wait for the crop layer's truncated input-stage preview, `apply` waits for its committed
  pixels, and the rest are captured on the next rendered frame.
- `slider` drives one gesture on a generated control: `{"action": "set-basic", "parameter":
  "exposure", "values": [0.25, 0.5, 0.75]}` sends one pointer move per value with the gated draft
  tick between them, exactly as a drag and the subscription produce them. `"release": true` ends it
  with the control's release, which commits once; `"cancel": true` ends it with Escape; neither
  leaves the gesture open and captures the frame once the draft has drained, so the pixels belong to
  the newest value it sent. A second `slider` step naming the same control continues the same
  gesture.
- `slider_draft` answers an open gesture's Changed elsewhere notice: `"discard"` or `"reapply"`.
- `field` types into one generated field: `{"action": "set-basic", "parameter": "exposure", "text":
  "1.5"}`, with `"submit": true` for Enter, which commits that one field without a draft.
- `reset` runs a declared reset: `{"module": "lightwell.basic"}` is the module's own header reset and
  `{"module": "lightwell.basic", "group": "Tone"}` is that control group's, found by its label.
- `view` sets the zoom through `view.set`: `{"zoom": "fit"}` or a percentage from 10 to 1600.
- `workspace` sets any of `state_panel`, `tools_panel`, `mode` and `thirds` through `workspace.set`,
  naming only the fields that actually differ from the session's own; captured on that round trip,
  or immediately when nothing differs.
- `preview` selects a loaded history entry by its sequence number (`{"sequence": N}`) or returns to
  the current state (`"current"`), exactly as a history row's click or "Return to current" does.
  Captured once its pixels reach the GPU.
- `palette` opens the command palette and types a query: `{"query": "text"}` captures the next frame
  with the palette open; `{"run": "text"}` additionally runs the first matching entry, settled the
  way that entry's own message would be.

A step that cannot be sent is recorded with `"status": "failed"` and its reason and still captures a
frame, so a refused step is visible in the evidence instead of missing from it.

The `crop` and `crop-draft` scenarios use this. `crop` commits a 16:9 `edit.crop-fit` and an
off-centre 7° `edit.crop`, then drafts on that layer, straightens to 12°, cancels, drafts again,
nudges and applies. `crop-draft` opens a neutral draft and exercises corner gestures, a declared ratio
preset, 100% and Fit, then applies. The runner checks the committed stack in each frame's state (one
crop layer keeping its identity, the payload that was sent, the revision each commit produced), that
the displayed image has the ratio the committed payload declares, and, on draft frames, that the
rectangle drawn at full opacity matches the captured draft rectangle, that all eight handles are
present and that the stage outside the rectangle is dimmed toward the window background. Each run
also writes `app/crop-checks.json` with the measured values and their tolerances.

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

`unavailable` is not one launch but two, since a module can only be disabled at startup. The first
opens the fixture and commits a 16:9 `edit.crop-fit` with every built-in module registered. The
second reuses the first launch's own catalog (`--catalog <dir1>/catalog.sqlite`) with
`--disable-module lightwell.crop` and reopens the same fixture, which the catalog dedupes to the same
asset by file identity, so its stack still names the now-unavailable crop layer. The runner checks
that the second launch's frame reports `state.render_error.code` `incompatible`, `state.notices`
naming "Preview is stale", the crop module listed unavailable in `state.modules`, no fixture colour
drawn anywhere in the photo surface, and the source fixture's hash unchanged throughout. It writes
`unavailable-checks.json` beside its own two launch directories rather than one `app/` directory.

On macOS, smoke, hardening, measurement and probe subprocesses always use the same background bundle as `develop --background`. Reports record `launch_mode`; reproduce through the harness to preserve focus protection. A native graphical session is still required. Windows and Linux retain direct launches; background behavior is not claimed there. Measurement launch times include the temporary bundle and executable copy, so they do not measure normal foreground activation.

Rules for any UI or image check:

- Capture after the intended generation is rendered, tied to state and logs, with explicit provenance. A PNG's existence is not a pass.
- Keep state checks, pixel checks with declared tolerances, UI review and native checks (dialogs, focus, resize, shutdown in a real desktop session) separate.
- Never report a screenshot as taken when capture is unsupported. A skipped or headless run is not native platform verification.
- Only synthetic fixtures in CI and shared artifacts. Routine logs use fixture identifiers, not private paths or EXIF. Keep personal photos in ignored `fixtures/jpg/` or `private/`.
- Record what was measured: host, build profile, fixture hash, backend, warm or cold cache. Native M4 timings are hardware evidence; VM or Xvfb runs are functional evidence only.

## Agent loop

1. Read the applicable spec and task, including any owner-decision gates.
2. Run `doctor` and the smallest checks appropriate to the change.
3. For UI or image changes, run a smoke scenario or the acceptance journey and inspect the capture as an image.
   On macOS, use the background harness or `develop --background` for every automated GUI launch; use the live API and renderer readbacks to drive and inspect it. Only perform foreground interaction checks when the owner explicitly requests them.
4. For changes under `crates/`, answer the [performance rules](performance-rules.md) checklist and run `editor-performance` on a generated 24 MP input in release.
5. Report exact commands, artifact paths, results and unsupported cases. Update task and feature status only when acceptance is met.

## Packaging

`cargo xtask package` builds an unsigned host development artifact: a ZIP on macOS and Windows or a `.tar.gz` on Linux containing `Lightwell/` with the executable, notices, `build.json` (source revision, dirty state, target, profile, binary and lockfile hashes) and `checksums.txt`. Run smoke against the packaged executable with `--binary`. Packaging is repeatable, not byte-reproducible, and inventory is not a completed license audit. macOS bundles are unsigned and not notarized. No signing, stores or auto-update exist.

## CI

`.github/workflows/check.yml` runs `cargo xtask check`, an optimized build and packaging on macOS arm64, Windows x64 and Ubuntu x64 with seven-day artifact retention, plus separate fixture and dependency-policy jobs. Linux additionally runs every smoke scenario against the packaged binary under Xvfb with software Vulkan and records runtime imports. Hosted results are compilation and functional evidence, never native desktop or GPU acceptance. Inspect actual run results for the tested commit; a configured step is not a passing result. Fresh hosted verification of the current tree and manual Windows/Linux desktop checks are open items in the [S0 follow-ups](../../tasks/implementation-s0.json).
