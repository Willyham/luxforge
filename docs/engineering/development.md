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
| The same without the slow tests and the doctests, what the quick tier runs | `cargo xtask check --quick` |
| The quick tier with its summary, with no release build | `cargo xtask verify --tier quick --output NEW_DIR` |
| A whole verification tier with one summary | `cargo run --release --locked --package xtask -- verify --tier quick\|rendered\|timing\|full --output NEW_DIR [--jobs N] [--binary PATH] [--manifest FILE]` |
| Individual steps | `cargo xtask check-repository`, `fmt`, `lint`, `test [--quick]`, `build [--release]` (the editor and the headless `luxforge-json`) |
| Run the editor, release build | `cargo xtask develop [--catalog FILE] [--open PATH] [--data-root DIR]` |
| Run a lightly optimized debug build, debugging only | `cargo xtask develop --debug ...` |
| Run an agent's editor check without taking focus (macOS) | `cargo xtask develop --background --catalog FILE [--open PATH]` |
| Display-independent acceptance of what `cargo test` cannot prove at the same layer: the Basic and histogram, field-patch conformance (in release), Presence, mixer and vignette, and masking chapters | `cargo run --release --locked --package xtask -- editor-acceptance --output NEW_DIR` |
| Core timing on a real-sized JPEG; `--lens-only` accepts JPEG or RAW and measures profile queries, commits, matched exact renders, point picks, serial export and cancellation | `cargo run --release --locked --package xtask -- editor-performance --source JPEG --output NEW_DIR [--samples N]`; for Lens, `--lens-only --source JPEG\|RAW` |
| Desktop slider/curve-to-presented-frame and settled-histogram timing, peak RSS, scratch and idle CPU; `--zoom` selects a percentage view, `--moving-pan` interleaves pan with a paced burst, and `--mode viewport` captures a held draft, pans, refinement, release and full-slot reuse at 100% or 200%, and `--mode crop-start` times opening a crop draft and reads its memory. `--presence` commits a Presence layer with all three fields at +100 before a drag, commit or crop-start. `--lens` selects the first eligible offline profile through the desktop control, with explicit acknowledgement for a JPEG; `--perspective` seeds +20 horizontal and -10 vertical. `--action`/`--parameter` measure another drafting slider in place of Basic exposure: a field-patch slider (presence, mixer, vignette, ...), or the RAW white balance `set-raw` `temperature` or `tint` over a RAW `--source`. | `cargo run --release --locked --package xtask -- editor-latency --source JPEG\|RAW --output NEW_DIR [--binary PATH] [--samples N] [--mode drag\|commit\|burst\|paint\|hover\|viewport\|crop-start] [--zoom PERCENT] [--moving-pan] [--control slider\|curve] [--action ID --parameter NAME] [--crop DEGREES] [--basic] [--presence] [--lens] [--perspective] [--mask] [--idle]` |
| Verify golden fixtures; generate 24 MP, 60 MP, the mixer and presence scenarios' own hue-wheel and gradient/edge/texture/flat workloads, and the `mask-range` scenario's own colour-chart patches | `cargo xtask fixtures`, `cargo xtask generate-fixtures [--output NEW_DIR]` |
| Adding a camera: download selected CC0 samples from the raw.pixls.us index, verify their SHA-256 and read each with the RAW adapter, or see why it refuses them | `cargo xtask raw-camera-metadata --index FILE --ids ID[,ID...] --output NEW_DIR [--max-source-mib N]` |
| Adding a camera: a DNG or TIFF's IFDs, geometry and calibration tags and opcode-list layouts, read-only | `cargo xtask inspect-dng --source DNG [--json NEW_FILE]` |
| Authentic RAW editor journey and reopen over one file the RAW manifest lists | `cargo xtask smoke --scenario raw-editor --source RAW --manifest FILE --output NEW_DIR [--binary PATH]` |
| Rebuild the pinned offline Lensfun resource | `cargo xtask lensfun-import --source UPSTREAM_DIR --output NEW_DIR` |
| Lens selection/export and marked-edge qualification through the JSON API | `cargo xtask lens-qualification --manifest FILE --edges FILE --output NEW_DIR` |
| Rendered Lens correction and Perspective workflow | `cargo xtask smoke --scenario lens-perspective --output NEW_DIR` |
| Rendered smoke scenario, needs a native graphical session | `cargo xtask smoke --scenario NAME --output NEW_DIR [--binary PATH]` |
| Every smoke scenario, with its launches and frame counts, what it opens and its window | `cargo xtask smoke --list` |
| A recorded smoke run's checks again, over a copy and without launching | `cargo xtask smoke --verify-only RUN_DIR --output NEW_DIR [--scenario NAME] [--source RAW]` |
| Rendered crop workflow and overlay | `cargo xtask smoke --scenario crop --output NEW_DIR`, `--scenario crop-draft` |
| Rendered workspace panels, mode, preview, the Transforms icon row, an agent's conflicting commit and palette; unavailable-provider notice | `cargo xtask smoke --scenario workspace --output NEW_DIR`, `--scenario unavailable` |
| Rendered Basic slider gesture: draft, commit, typed value, undo, reset and an agent's conflicting commit | `cargo xtask smoke --scenario basic --output NEW_DIR` |
| Rendered Basic panel on a JPEG: all three groups, the White balance group's four controls (Temperature, Tint, Neutral picker, As shot) with As shot sending Basic's own 0 and 0, historical values, a group reset, the neutral picker, As shot after a warm drag, and the default screen with Basic expanded and every other section collapsed | `cargo xtask smoke --scenario basic-panel --output NEW_DIR` |
| Rendered histogram, clipping overlays, pointer readout and a drafted frame | `cargo xtask smoke --scenario histogram --output NEW_DIR` |
| Rendered Basic composed with crop and straighten | `cargo xtask smoke --scenario basic-crop --output NEW_DIR` |
| Rendered restart: a Basic edit committed in one launch and reopened in the next | `cargo xtask smoke --scenario basic-restart --output NEW_DIR` |
| Rendered Presence: section expand, a Clarity drag and cancel, Texture and Clarity each committed at Fit and 100%, Dehaze at both signs, all three fields at once through the raw API and the module reset, over a generated gradient/edge/texture/flat fixture | `cargo xtask smoke --scenario presence --output NEW_DIR` |
| Rendered Colour mixer: section expand, a Red hue drag and commit at Fit and 100%, a Saturation group reset, a stronger hue shift and the Saturation and Luminance tabs, over a generated hue wheel | `cargo xtask smoke --scenario mixer --output NEW_DIR` |
| Rendered Vignette: section expand, an Amount drag and commit at Fit and 100%, roundness and feather extremes, a post-crop recentre and the module reset | `cargo xtask smoke --scenario vignette --output NEW_DIR` |
| Rendered percentage zooms: 50%, 100%, 120%, 800% and 1600%, pans to the centre and the far corner at 1600%, and idle checks at Fit, 100% and 1600%, over the generated 24 MP and 60 MP JPEGs, one launch each | `cargo xtask smoke --scenario zoom --output NEW_DIR` |
| Rendered 100% viewport with two masks and a rotated crop: draft, pan, refinement, release, settled reuse, history and overlay identity; GPU draw counters must record no blank or stale photo | `cargo xtask smoke --scenario viewport-region --output NEW_DIR` |
| Rendered explicit exact fallback for an estimate after a spatial layer, with two masks, pan, draft and release; GPU draw counters must record no blank or stale photo | `cargo xtask smoke --scenario viewport-fallback --output NEW_DIR` |
| Rendered ordinary 100% to Fit refit after 1 s of quiet: disable evidence ticks and frame capture before changing view, check requested GPU draw identity and blank count at the deadline before capture resumes, then capture the resulting frame | `cargo xtask smoke --scenario viewport-idle-fit --output NEW_DIR` |
| Rendered Presets: section expand, an XMP and a Luxforge preset imported, each applied from its row, undo, the create form filled and submitted, a native preset applied to the Original, `preset.list` through the `api` step and a delete through the row menu | `cargo xtask smoke --scenario presets --output NEW_DIR` |
| Rendered export over the EXIF orientation 6 fixture, brightened and cropped to 16:9: the title bar's Export menu open, the displayed entry exported with metadata stripped and again keeping it into the run's evidence directory (only the save dialog is bypassed), each file decoded independently for its dimensions against the captured output stage, its byte length, ICC profile, APP1 segments and EXIF orientation, and a Keep metadata export to the stripped file's name refused with the status bar's reason and the file unchanged | `cargo xtask smoke --scenario export --output NEW_DIR` |
| Rendered Performance section: open and sampling from the launch, a filled window, a straighten and a commit of all three Presence fields whose render is listed as long work and then as finished, collapsed and asleep, then reopened on a fresh window, over the generated 60 MP JPEG, with the editor's memory read by the runner from outside the process; `--source RAW` runs the same over a RAW photograph, outside `rendered` | `cargo xtask smoke --scenario performance --output NEW_DIR [--source RAW]` |
| Rendered Basic section over a supplied RAW file, in place of a RAW section, which no frame lists: the White balance group's four controls in the JPEG's order, each the RAW development's (Temperature in K and Tint over `set-raw`, the Neutral picker entering the sensor pick, As shot sending `set-raw {white-balance: as-shot}`); a Temperature drag left open and then released, at Fit and at 100%, whose drafted frame differs from the one before, is labelled approximate and adopts no histogram, and whose release's exact frame is the first drawn after the commit and carries its own report; at Fit its moving approximate frame is within 10% of the drag's own change and within one code on average, each averaged over the photograph alone, while at 100% the 10% gate compares the held approximate draft after full-detail refinement under the shared 120 ms quiet policy and before release (the quiet interval runs from the drag's last input, so the refinement can begin while the drag's frame is still being written: the moving checks read the drag's events up to its settle, and the held checks read the rest with the wait's); moving 100% separately verifies viewport identity, approximate label, visible response and refinement, and records softness without a numeric threshold; a highlight-clipped Bayer scene (at least 1% of its Bayer sites at 0.99 of sensor white or above under the drags' gains, with no DNG correction after the demosaic) records a missed white-balance accuracy limit with the exception, its clip share and the limit instead of failing, while every other check stays a gate; each drag keeping the tint in force (the first, from As shot, the core's as-shot tint) in the committed payload and in the Tint field throughout; then a double-click on Temperature, Tint and Exposure: the first press's committed jump and the reset that follows it, each checked as two entries with the reset sent against the jump's revision and never refused — Temperature and Tint back to As shot (`set-raw {white-balance: as-shot}`, the entry labelled Reset White balance, both fields showing the core's as-shot equivalent, checked back through the forward map), Exposure back to 0 EV; Basic's dot, absent on the untouched photograph, present after the committed custom temperature and absent again at As shot and 0 EV; `W` entering the RAW development's sensor pick with Basic's Neutral picker selected, and Escape leaving it; then a crop drafted on the RAW's whole input stage, 16:9 and straightened by 7°, whose draft is one picture (an 80 × 60 grid of stage points is compared with the unstraightened draft at the same points; of the at least 1,200 whose scene is 8 codes or more from the canvas colour there, after the draft's dimming outside the crop rectangle, under 0.5% may show the canvas, so dark scene content the canvas's colour is never taken for a gap; `STRAIGHTENED_RECORDED=DIR cargo test -p xtask straightened_drafts_recorded -- --ignored` judges the straightened draft of each recorded `DIR/raw-panel*` run this way, whatever an earlier check found), applied at Fit, read at 100% through two pointer readouts and replaced by a −12° 3:2 `edit.crop-fit` at 100%: no step logs a failure, every committed frame shows the current entry at the output its payload declares, placed and centred at Fit within 4 px, and each readout's codes are the canvas's own at that stage pixel within one code; not in `rendered`, because no RAW photograph is checked in | `cargo xtask smoke --scenario raw-panel --source RAW --output NEW_DIR` |
| Rendered module capabilities: settings, a profile, its key, a download grant and install through `api` steps, the photo-data consent denied then allowed, a task with progress, Apply and a refused task through the desktop, against a loopback proof endpoint | `cargo xtask smoke --scenario capabilities --output NEW_DIR` |
| Rendered Masks panel over the photograph the design boards use: Sky, Face (a radial, a subtracting brush of two strokes and an intersecting luminance range) and Foreground built through the panel and renamed through `mask.rename` and `mask.rename-component`, Foreground's overlay hidden with its eye, Face's amount, Exposure and Clarity through it; then the Brush section armed and put down, a held stroke's draft bar, the New mask menu and Escape, Radial 1's fields, the overlay in each mode and both tints, a hovered row, and Radial 1 reopened with its grip swung to −12°, the mask-mode board's own state. Every frame's list, open mask, selected component, overlay and mode are checked against the plan, and the draft bar, scope chips, dot and bound layers by state | `cargo xtask smoke --scenario mask-panel --output NEW_DIR` |
| Native masking interaction regressions: unplaced creation, selected/armed targets, live and committed brush flow/feather, deliberate hiding and analytic live-gradient coverage under rotated crop at Fit/100% | `cargo xtask smoke --scenario mask-interactions --output NEW_DIR` |
| The capability framework's own costs (registration, capability reads, a task, artifact publish, cancellation), release only | `cargo test --release --locked -p luxforge-core --lib capability_timing -- --ignored --nocapture` |
| How promptly a cancelled 24 MP render stops, in the transform pass and mid colour chunk, against its 25 ms bound, release only | `cargo test --release --locked -p luxforge-core --test cancellation -- --ignored --nocapture cancelled` |
| Inspect a capture | `cargo xtask check-capture --image PNG [--orientation N]` |
| Process failure checks; macOS measurement, `--samples` defaults to 5 launches per workload | `cargo xtask hardening --binary PATH --output NEW_DIR`, `cargo xtask measure --binary PATH --output NEW_DIR [--samples N]` |
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
| `one-spatial-entry` | Keying the estimate store by a domain's prefix, `.estimate_prefix(`, anywhere but once, in `SpatialEntry::globals` in `render/pipeline.rs` | Core production code |
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
| `test-gates` | `Condvar`: a test's own gate, outside `luxforge-testbase`'s `Gate` and the core's production blocking points (the source worker's plane gate in `source.rs`, the latest-job worker in `latest.rs`, the point-query worker in `api/owner/point.rs`) | `crates/`, tests and comments included |
| `one-distribution` | A second percentile definition outside `luxforge-testbase`'s `Distribution` (`distribution.rs`): the shapes a hand-written one took (`fn percentile`, `let percentile`, `fn median`, `let median`, `fn p50`, `let p50`, `let p95`) and a nearest-rank rank computed again (`div_ceil(100)`) | `crates/` and `xtask/`, tests and comments included |
| `thread-spawn` | `thread::spawn`, `thread::Builder` and `thread::scope` outside the declared worker homes: the core's source worker and owner loop, point-query worker, API transport threads, the job table's lanes and latest-job worker; the desktop's diagnostics log writer; the widget crate's GPU retirement worker; the test kit's process and server threads; `verify`'s component pool | Production code under `crates/` and `xtask/` |
| `one-photo-locator` | `["photo_rect"]`: a scenario reading the rectangle the editor records drawing the photograph in for itself, outside the one locator over it (`Frame::photo_rect`, `photo`, `visible_photo`, `photo_edges` in `xtask/src/scenario/pixels.rs`) | `xtask/src/`, tests included |
| `editor-launch` | An editor argument (`"--evidence-dir"`, `"--evidence-script"`, `"--data-root"`, `"--catalog"`, `"--open"`, `"--developer"`, `"--disable-module"`, `"--proof-endpoint"`, `"--window-size"`), `spawn_editor` or `editor_args` outside the scenario library's launch envelope (`xtask/src/scenario/launch.rs`) and the hidden-window flag's home (`xtask/src/launch.rs`) | Production code under `xtask/` |
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
| `headless-cli` | A normal dependency of `luxforge-cli` on the GUI stack (`iced`, `iced_wgpu`, `wgpu`, `rfd`, `luxforge-ui` or `luxforge-app`), so building the headless `luxforge-json` builds no window, renderer or dialog crate |

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

Timing runs wait until feature work is complete: the `timing` tier, `editor-performance`,
`editor-latency`, `measure` and every `--samples 30` distribution. A figure taken
mid-implementation measures code that is about to change, on a host loaded by builds and tests. The
exception is work whose subject is performance (a budget, a regression, an optimization), where
measurement is the feedback: iterate with the one targeted command at its default sample count, and
take the claimed distribution and the before/after once, at the end.

When work is split across agents, each agent runs targeted tests while working and `quick` at
hand-off; the integrator runs `rendered`, `timing` and `full` once, on the integrated branch.

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
| `rendered` | the whole `check`, `editor-acceptance` and all 37 smoke scenarios, including `zoom`, `presets`, `export`, `gallery`, `controls`, `capabilities`, `performance`, `lens-perspective`, the three viewport scenarios and the five `mask-*` ones, through a bounded pool |
| `timing` | the whole `check`, `editor-acceptance`, then `editor-performance`, `editor-latency` and `measure`, in that order, serially, after everything else in the tier and behind the host-wide timing lock |
| `full` | rendered plus timing plus `hardening`, plus, with `--manifest FILE`, a `smoke --scenario raw-editor` run per manifest source (`raw-editor-<id>`), the owner-supplied authentic RAW tests via `raw-authentic`, a `smoke --scenario raw-panel` run per manifest source and one `smoke --scenario performance` run over the first manifest source |

When to run each tier is in [when to verify](#when-to-verify). `hardening` needs only `--binary` and
runs in `full` whether or not a manifest is given. Without a manifest, `full` lists `raw-editor` and
`raw-authentic` as `skipped` with the reason `no --manifest`, and adds no per-source `raw-editor`,
`raw-panel` or RAW `performance` components at all, since there is no source to run them over.
`verify` reads the manifest through the one RAW manifest reader, so a manifest the `raw-editor`
scenario would refuse is refused before anything runs. `raw-authentic` runs the
`#[ignore]`d authentic-file tests in `luxforge-raw`'s `real_files` and the `raw::` module of
`luxforge-cli`'s `json_cli` (the ones that need only `LUXFORGE_RAW_OWNER_DIR`, not `real_files`'s
separate CC0-fixture test) with that variable pointed at the directory the manifest's own sources live in.

A skip is never a pass: a tier with any component `skipped` or `not_run`, and nothing failed
outright, is `incomplete` rather than `passed`, naming which components and why in the headline and
in `summary.json`'s `incomplete` list, and its process exits with its own code (currently `3`),
distinct from `0` (passed) and the ordinary-failure exit code a real component failure uses.

Above `quick`, the command builds `luxforge-app` and `xtask` once in release, then runs each component as a child
process of the release `xtask` executable with its console output in `<out>/<component>/console.log`
and its own evidence in `<out>/<component>/run/`. `--binary PATH` is forwarded to every component
that takes one; without it the executable just built is passed explicitly, so every component
measures the same file. The rendered and timing tiers run `generate-fixtures` first when any
generated fixture — 24 MP, 60 MP, hue-wheel, presence or range — is missing. A component that has
stopped making progress is killed after twenty minutes and recorded as `timed_out`. `quick` builds
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
`editor-latency`'s `latency.json` in every mode (its viewport scalar and GPU counters included) and
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

## Running the application

`cargo xtask develop` starts the editor. It owns the catalog (`--catalog FILE`, defaulting to the platform configuration directory), offers native Open with Cmd+O or Ctrl+O and starts an authenticated loopback JSON service. `--data-root DIR` isolates config, data and log paths. The application also accepts `--window-size W H` (320 to 4096 logical), `--developer`, `--disable-module MODULE_ID`, `--proof-endpoint URL` (registers the developer capability proof against that endpoint; refused without developer mode), `--hidden-window`, `--evidence-dir NEW_DIR` and `--evidence-script FILE`. `--hidden-window` creates the window invisible: it owns a real surface and renders and captures through it exactly as a visible window does, but the window server never places it on screen. Every automated editor launch the harness makes passes it; `develop` in either mode never does. `--developer` lists proof and diagnostic modules, which are hidden by default so the workspace stays a photo editor; the API is unaffected. `--disable-module MODULE_ID` registers that built-in as unavailable, keeping its effect identities readable so a stack that uses it reports the unavailable effect instead of rendering without it; an unknown identity is a startup error. Evidence mode is the same editor driven by the harness: each `--open` goes through the ordinary import call into a catalog created inside the new evidence directory, a window frame is captured after each outcome, the script's steps then run with a frame each, and the run exits after writing its results. Manual Open is disabled during collection, and `--open` may repeat only with `--evidence-dir`.

The headless owner, `luxforge-json`, reads one JSON request per line. It is the `luxforge-cli` crate's binary, which builds without the GUI stack: `cargo xtask build [--release]` builds it with the editor, and `cargo build --release --locked -p luxforge-cli` builds it alone.

```sh
target/release/luxforge-json --catalog /path/to/catalog.sqlite < requests.jsonl
```

Start with `schema.list`. Request shapes and live-session behavior are in the [user guide](../user-guide.md). `--data-root DIR` puts module settings, grants and installed resources under that root instead of the platform directories, `--secret-store memory` keeps module secrets for the process only instead of the platform store, `--permission-authority` lets this client grant module consent (an explicit local setup step; without it `module.permission.grant` is `forbidden`), `--developer` serves the test modules, the pixel and controls proofs, as the desktop's flag does, though a debug build is never in developer mode without it, and `--proof-endpoint URL`, which needs `--developer`, registers the capability proof module. The desktop's own client always has that authority; an evidence run keeps module state inside its evidence directory with an in-memory secret store, so no automated run touches the person's configuration or login keychain. Only one process owns a catalog at a time; a second instance exits with an explanatory error. Diagnostics go to stderr, or to isolated logs under an explicit data root, never to protocol stdout. Editor mode writes only the catalog and a temporary live-session file beside it; the source original is never written.

On macOS, `develop --background` builds the selected profile and runs a temporary copy in an `LSBackgroundOnly` app bundle, preventing desktop activation. Use an isolated catalog or `--evidence-dir NEW_DIR` for automated checks. The live API and native GPU renderer remain available; this mode is for API and capture work, not keyboard, mouse or native-dialog checks. The bundle is removed after exit, and the original executable and packaged app are untouched. Restricted tool environments must permit macOS LaunchServices/window-server IPC: a background process can otherwise stall before image work, with only startup/open-request events and idle source/catalog workers. Retry with the required host access rather than activating the window. Ordinary `develop` remains an interactive launch. `--background` fails explicitly on other platforms.

### Browsing the component gallery

Debug builds expose the title-bar **Developer** button automatically. To inspect the gallery in
an optimized build, run `cargo xtask develop --developer` (automated launches add `--background`).
Its page chooser and Previous/Next controls browse thirteen pages; Back to editor or Escape returns.
The `gallery` smoke covers all 99 named widget states and the return to the unchanged editor via
the same view message as the button; the page is desktop view state, so the session's workspace
stays unchanged throughout.

## Rendered evidence

Smoke runs the built or packaged editor through a deterministic evidence sequence (repeated `--open`, evidence directory, fixed window size, bounded deadlines); there is no separate viewer, so the captured frame is the editor window with its sidebar. Every scenario is one row of the runner's table, which `cargo xtask smoke --list` prints with its launches and frame counts, what it opens and its window; generate the large fixtures first. Each launch has a plan: every frame it captures, in order, with the script step that produces it and what that frame must show. The script the launch runs and the number of frames it must capture are derived from the plan, and every frame of every scenario is checked against it before the scenario's own checks: its run identity, backend, renderer-readback provenance and state file; for a scripted frame, the step the editor recorded equal to the scripted one as its parser reads it back, listed in the run's script and log and `sent` unless the plan expects it refused; an input error exactly when the plan expects a refusal; then the step's own expectations, such as the commit it makes, the history label, a layer's payload or identity, a field's text, the draft, the sections expanded, what the Masks panel shows (the masks, the open mask, its components' names, modes and kinds, the selected row, the overlay control, its menu and the Brush section), the notices, the status line, workspace fields such as the canvas mode and the overlays, and the zoom. Each launch's `plan-checks.json` records what every step was checked against, and a failure names the step. What a plan cannot say, mostly what the pixels show, a scenario checks itself, and records through one recorder, `scenario::Checks`: each pixel claim is a comparison of two readings under a stated tolerance, recorded with both readings whether it holds or not, beside notes of what a frame shows, all written once as `<scenario>-checks.json`. Ignored `xtask` tests check a change to the plans or to how scripts are written: `SCRIPT_DUMP=DIR cargo test -p xtask dump_script -- --ignored` writes every launch's script and argument list as a launch writes them, every script and argument list `editor-latency` can write (its viewport mode, `--zoom` and `--moving-pan` included), and the argument list of every `measure` and `hardening` launch, to compare against the same dump from before the change (a row whose plans read more than its sources, such as `raw-editor`'s neutral point, is dumped over the placeholder its table plan names), and `SMOKE_RECORDED=DIR cargo test -p xtask mutations -- --ignored` (`DIR` absolute, since the test runs in `xtask/`; it fails when no run is found) replays each `DIR/smoke-<scenario>/run`, a supplied source given again as the one its first launch opened, with its plan's last step removed and with its first expected label changed, and fails unless each replay fails on the frame count and on the named step. Each run writes `result.json`, `app/events.jsonl`, `app/state.json`, `app/frame-*.png` (window-renderer readbacks, not OS screenshots), `subprocess.log` and `reproduce.md`; a scenario of several launches writes each one's evidence to its own directory and log instead, and `result.json` lists every editor process a run started under `launches`, with its command and exit code, which is what `verify` counts. An editor's diagnostics log holds at most 4,096 events: past that it counts what it drops, writes one `diagnostics_truncated` record naming the count when it finishes, and an evidence run whose log was truncated fails exactly as one whose log could not be written does. `smoke --verify-only RUN_DIR --output NEW_DIR` copies a recorded run, less the checks files its checks wrote, and runs the scenario's own code over the copy with each launch taken as the recorded one: the checks files it writes are that run's checks again, and `replay.json` is the verdict. The scenario comes from the run's `result.json` unless `--scenario` names it, and a `--source` run is given its source again. The `capabilities` scenario's endpoint record and secret scan are what the live run's own process saw, so a replay carries them from the recorded checks. Each frame records `surface_columns`, the physical x range of the photo surface derived from the editor's layout constants, `canvas_rect`, the canvas region between the panels and the bars as `[left, top, right, bottom]` physical pixels, which at a percentage zoom is exactly the scrollable the photograph pans in, `fit_rect`, the area Fit lays the photograph out in, and `photo_rect`, the rectangle the canvas draws the photograph into, right and bottom exclusive and snapped as the photo surface snaps it, carried past the capture's edges by a percentage zoom and `null` when no photograph is drawn. Scenarios locate the photograph by `photo_rect` alone: `Frame::photo` requires it inside the capture and holding a drawn picture, `Frame::visible_photo` clips it to the canvas, and where a claim is about placement `Frame::photo_edges` ties it to the capture, the pixel just inside the middle of each edge drawn and the one just outside the canvas surface; the runner verifies fixture colors, Fit geometry and centering within that range, generation and state, backend, exit status and unchanged source hashes before writing `passed`; blank, stale or missing frames fail. The `render_ready` event marks the upload of the open request's preview raster, which is when a frame becomes capturable. A script step's `script_step` record is followed, once what the step waits for has happened, by one `script_step_settled` record naming the wait (`waited_for`, such as `preview`, `slider_draft` or `session`) and the outcome that ended it (`by`, such as `presented_photo`, `presented_draft`, `draft_refused` or `session_answered`); a refused step records `script_step_failed`. The desktop's seams report those outcomes as typed values (`app/outcome.rs`) that only the evidence driver reads, and the desktop builds an event only when it has a log to write it to: an evidence directory, `--data-root`, or stderr when either was asked for and the log could not be opened. Single-open evidence has a 25-second application deadline; multi-step evidence scripts have 60 seconds for repeated RAW redevelopment. Smoke retains its 35-second process deadline; the RAW editor journey has a 70-second process deadline. These are harness hang bounds, not interactive latency targets.

At Fit, and at any zoom that draws the stage smaller than itself, the frame a scenario captures is
the **display proxy**: the whole recipe rendered against a source downscaled once to the photo area,
which is the size the display was going to minify the exact render down to anyway. `preview_displayed`
therefore carries `proxy`, `proxy_dimensions` (the proxy source's own size, null when there is none),
`proxy_built` (the proxy source was built for this frame rather than taken from the queue's cache)
`reason` (`"zoom"` when the frame is a retained raster a zoom change needed rather than a
render) and `render_ms`: the preview worker's own time for the phase that produced those pixels —
the proxy build when that frame built it plus the render, or the exact render plus the reduction —
excluding the queue wait, source preparation and the hand-over, and for a `"zoom"` frame the time
recorded with that retained raster. It is the figure the status bar states as
`Approximate render · N ms` or `Exact render · N ms` according to the presented frame. Its `dimensions` stay the exact output stage's, which is what picks, the
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
`render_ready`: there is no upload step to time. The clipping overlay, the mask coverage and the
crop draft's input stage are drawn by the same surface from frames of their own, handed over in
the update that has them: `clipping_overlay` and `mask_overlay` are emitted in that update. For
ordinary photos with clipping enabled, capture waits for a GPU draw containing both the requested
photo identity and that request's assigned clipping frame version (`drawn_clipping_version`); if
the clipping version changes while a screenshot is in flight, capture retries. Empty, crop, gallery
and render-error captures do not wait for this pair. If clipping derivation or presentation fails,
the active script step is marked failed and the refusal frame is captured. Mask coverage still
settles a waiting step in its update, and the crop draft's open frame is drawn over its input
stage in the update that takes that stage up. `preview_exact_adopted` records the exact phase of such a job
being taken up without an upload, with that phase's own `render_ms`, `preview_exact_cancelled` records one a newer request superseded, under its own generation and with `draft` when it was a crop draft's input stage, and
`clipping_overlay` carries `approximate` while the mask is derived from the proxy on screen rather
than from that exact raster. `preview_failed` records every failed preview of the displayed state,
with its entry and reason, and a scripted step waiting for the newest preview's pixels ends on it
and captures the failure. `preview_withdrawn` records a failed preview whose target was not the
picture on screen: that picture, named with the target, is taken off the surface with everything
derived from it, so `state.surface.raster` and `state.stack.displayed` are null until a frame of the
target renders, and `crop_draft_failed` records a draft whose input stage could not be rendered or was superseded before it rendered (`error_code: cancelled`, with that job's `generation` when it had one). A
crop draft's `state.crop.input_stage_loaded` says its input stage is on the surface. `state.json` carries `proxy: {eligible, declined, approximate, dimensions, bounds,
presented}`, so a stack that took the exact path — an ineligible layer, a stage already inside the
bounds, a failed build — says so rather than being silently identical, and `status_bar: {readout,
render, render_ms, render_proxy}`, what the bar drew and the figure behind it. The `histogram`,
`basic`, `large24` and `large60` scenarios check that every `preview_displayed` carries a finite
`render_ms` below 5 s and that each captured status bar states one of those figures in the editor's
own wording, with `(proxy)` exactly when the frame on screen is the proxy.

### What editor-acceptance proves

`editor-acceptance` keeps only what `cargo test` cannot prove at the same layer: a chapter or step
exists only if no `cargo test` proves it at that layer. Four things remain, each driven through the
JSON method table with `OwnerHandle::call` as an independent client, against its own catalog inside
the run's output directory: the [field-patch conformance suite](#the-field-patch-conformance-chapter)
in release, [Basic's numerics on the photo fixture against the independent
reference](#the-basic-and-histogram-acceptance-chapter), the [placement of Presence, the mixer and
the vignette](#the-field-patch-conformance-chapter), and a [masked catalog reopened through a fresh
owner](#the-masking-acceptance-chapter). Everything the core's own tests prove stays there: history
order, the read-only preview, undo, redo and restore, the orientation layer, the crop and reopen are
`editor::history`, `editor::plan`, `modules::transform` and `modules::crop`'s tests, the module and
method discovery is the [descriptor snapshot](#the-built-in-descriptor-snapshot) and the method
table's, and the mask commands, their refusals, a disabled maskable module and a missing or changed
original are the `mask` test binary's, the command family's own tests and the conformance suite.
A new step here needs a property that only a release build, an independent oracle, a second process
lifetime or a fresh owner can show.

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

Basic, Presence, the colour mixer, the vignette and the developer controls proof are one
declarative field-patch module each, and the host behaviour they share is proved once, for every
module the built-in registry and the controls proof hold in that shape, by one suite in
`crates/luxforge-core/tests/modules/conformance/`. The suite finds the modules from their
descriptors — one effect, one `patch` action whose parameters are all fields with defaults (a
number, integer, boolean, enum, colour or curve), and the parameterless action the module reset
names — and derives every payload it sends from the declared field table, so a new field-patch
module is checked the day it is registered. It refuses to run when it no longer recognises one of
the four built-in ones or the controls proof, whose fields are the non-numeric kinds. The controls
proof's layer changes no pixel, so it is held to the in-process checks below and to compiling to
nothing and sharing the source allocation whatever it holds, not to the pixel consequences and the
journey through the method table. Every `patch: true` action of every registered module, `set-raw`
included, keeps the generic patch check's rules: an empty patch is filled with nothing, a declared
default alone is exactly that field, and a value its declaration refuses is refused by name. The
same function runs twice: as the core's `modules` integration test (`field_patch`) in the dev
profile, a [slow test](#how-check-runs-the-tests) the quick tier leaves out, and in release inside
`editor-acceptance`, which records what it returns under `field_patch_conformance` in `result.json`.
Each module runs against its own new catalog under the run's `field-patch-conformance` directory, and
a failure names the module, the step and the property that broke.

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
last and recentred on the stage each crop update produces.

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
rename, a masked Basic layer, a masked Presence layer and a masked mixer layer, and a duplicate that
copies the bound layer. It then stops. A fresh owner over the same file returns the same revision
and current entry, the masks, components, payloads and bound layers by identity, and the same
`render.sample` values at six positions across the frame. Everything lands in `result.json` under
`masks`, and any mismatch fails the command.

One contract shapes how it reads pixels: it uses `render.sample` and never renders a recipe in
process. A brush component's payload holds its strokes by content address and the resolved strokes are
never serialized, so a recipe fetched over JSON has addresses and no points and rendering it outside
the catalog that holds the store is refused by name. That is the retention contract working. The owner
has the store, so the owner is asked.

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
  references the same way, and rejects a script that sets either; a method that is not an edit of
  the open asset fails the step. The desktop sends nothing itself: the owner wakes its event sync
  for the other client's change, the sync reads it back as a change made elsewhere, and the frame
  is captured once the agent has its answer and the frame of the entry that request committed has
  been presented (`waited_for` `agent`), so an open draft is conflicted exactly as another client's
  commit conflicts it. An answer that commits nothing is captured on the next frame, and a refused
  one is recorded as failed. The event sync runs in every evidence run as it does in a session; it
  reads only what the owner wakes it for, which is another client's change or a job of the
  desktop's own that ended (an import, an export, a module task).
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
  session follows, any other bound key on the next frame, and a key the table does not bind fails.
- `wait` (`{"ms": N}`, 1 to 10000) asks nothing of the editor for at least that long and then
  captures. The evidence run's own 250 ms tick keeps rebuilding the view meanwhile, so the frame
  shows what repeated rebuilds with nothing new to show did.
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
  canvas publishes a move, and is captured once the exact readout has answered from the matching
  retained raster or `render.sample`.
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
- `performance` opens or closes the state panel's Performance section through its heading's own
  message: `{"expanded": true}` or `{"expanded": false}`. Opening it, with the state panel shown, is
  captured once the section's first `resources.read` and `activity.list` have answered, so the frame
  shows figures rather than dashes; closing it, opening it under a hidden panel and asking for the
  state it is already in are captured on the next frame. The section is open at every launch, so
  every scripted run samples once a second unless its script closes the section.

- `capability` drives one gesture on a module's task control or the consent notice through the message that control sends: `{"module", <one of>, "wait"?: false}` with `task` (`{task}`), `consent` (`"allow"` or `"deny"`), `apply` or `settle`. A module's settings, profiles, secrets, grants and resources are set with `api` steps. A step is captured once its round trips have answered and the jobs it started have finished; `"wait": false` captures as soon as a started job reports progress, and a later `settle` captures once the module's jobs are done. `state.json` carries a redacted `capabilities` summary per module — the settings as `module.settings.read` answers them (a secret only as `secret_present`), each resource's state, the jobs, each task's newest run, the permission counts, the open consent and the block's status line — and stack layers carry their `artifacts`.

A step that cannot be sent is recorded with `"status": "failed"` and its reason and still captures a
frame, so a refused step is visible in the evidence instead of missing from it.

Each scenario's plan and checks are documented in its own module (`xtask/src/*_smoke.rs`, with the table of rows in `xtask/src/smoke.rs`), and `cargo xtask smoke --list` names every scenario with what it proves, the launches and frames it makes, what it opens and its window. Most write the values they measured and their tolerances to a `*-checks.json` file in the run's output. A scenario's script is the steps above, so what a step does is specified here and what a scenario asks of it is in the scenario. The gallery board and the controls scenario's generated panel are different widths; both need visual review alongside their automated checks.

`editor-latency --control curve` measures the controls proof's middle-point drag with the curve editor visible. The proof's colour stage is identity; this measures the control, query, draft, preview and upload path, not a future Tone Curve image algorithm. Slider remains the default workload. Both use the same provisional 100 ms p95 interaction threshold and retain all samples.

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
open, so the step settles only once the gesture has drained: one input, one `draft.set`, one preview
job, one frame, with nothing from the previous input still in flight. `slider_draft_set` gives the
input's time, `slider_draft_preview` names the preview generation that `draft.set` produced, and the
`preview_displayed` of that generation is when that raster became the photo surface's source.
Presented therefore means that update, whose redraw draws the frame, not display scanout: the
figures are an upper bound on the editor's own work and a lower bound on what an eye sees. The
report has no upload row for the same reason: the photo surface writes its texture in the frame
that draws it, so `render_and_upload` covers the render and the hand-over together. The measured window is
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
`--action`/`--parameter` too. `--lens` selects the first eligible profile through the Lens control,
carrying the assume-uncorrected acknowledgement explicitly. `--perspective` commits +20 horizontal
and -10 vertical before measurement. `--crop DEGREES` commits a straightening 16:9 crop first, so
the measured stack carries the fused geometry resample as well as the colour pass. These flags
apply to drag, commit, burst, viewport, paint, hover and crop-start. `--basic` commits a Basic layer
with every field non-neutral first, so each measured frame runs every one of the module's colour
units. `--idle`, available in drag and commit modes, reopens the gesture's own committed catalog
and source in an ordinary launch. After the exact histogram is adopted and one second of settling,
it measures CPU time and sampled RSS for thirty seconds with the same Lens, Perspective, crop and
colour stack. This includes the open Performance section's sampler. The harness then stops the
process; this is no clean-close check. `latency.json` and `resources.json` keep every sample,
scratch high-water and correlated state. Native GPU allocation, when the platform provides it,
and the photo surface's full, region, retiring and crop-stage texture bytes are captured levels
from the gesture, not idle-process measurements or peaks. Surface bytes exclude backend staging;
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
real-time coalescing of that pace), `presented_frames` (every `preview_displayed` over the run,
drafted and committed alike), `cancelled_exact` (`preview_exact_cancelled` events: full-resolution
phases a newer request superseded, which carry no frame) and `proxy` (the last presented frame's
`proxy`/`proxy_dimensions`). Its figures are rows: `presented_fps` (`presented_frames` divided by
the seconds from the first `slider_step_value` to the last of them), `staleness_ms` (each presented
drafted frame's own `slider_draft_set` time to its `preview_displayed` time, paired by generation
exactly as drag mode pairs them), `frame_gap_ms` and `max_gap_ms` (the intervals between
consecutive presented drafted frames), the GPU counters `draw_encoded_frames`,
`photo_texture_writes` and `photo_upload_bytes`, and the sampled resources.
With `--zoom PERCENT --moving-pan`, the same paced tick also moves the photo scrollable on a path
across and back over the image. Without `--moving-pan`, a zoomed burst holds a fixed viewport and
its script can run against the pre-viewport binary for a like-for-like baseline. The moving-pan
report counts `slider_step_pan` events and records the region events' content,
revision, quality, generation and geometry alongside the captured surface GPU counters. A burst
still measures adoption rather than scanout; multiple adoptions can occur before one draw.

`--zoom PERCENT` sets the view before the measured gesture. `--mode viewport` requires `--zoom
100` or `--zoom 200`; each journey opens a slider draft, pans, leaves the draft quiet for 1.5 seconds,
moves the slider again, pans and pauses again, then releases and pans after the full report
settles. The captured states and `preview_displayed` events must name interactive and exact
regions for each draft revision. The first viewport-only frame keeps the histogram updating;
release must produce a current full-image histogram. The final settled pan must draw the same
full texture without another photograph write. `--samples` is 1 to 60 sequential journeys, with
one background editor at a time. Each imports and prepares its source before measured inputs;
filesystem caches are warm, while prepared buffers belong to that journey. A journey contributes
two observations each for pan-to-interactive adoption, pan-to-exact adoption and refinement-request
to exact adoption. The first two start at the pan's scripted input; the refinement row starts at
the quiet-settlement request and excludes the preceding quiet delay. Every pair must name its
own requested generations and the same draft revision, entry and source. Thirty journeys therefore
give sixty pan and refinement observations. `latency.json` pools actual observations into
distributions; `viewport-NNN.json` and the matching evidence directory retain each journey's
frames, regions, draw/write counters, RSS, scratch and GPU levels. Readbacks and step settling occur
between measured pans. Aggregate reliability uses the highest observed load before or after any
journey, with each endpoint retained. `preview_displayed` remains an adoption timestamp, not GPU
upload or display scanout. A binary with no region events leaves an `unavailable` journey report
and fails the aggregate run.

`--mode paint` measures a **paint** gesture instead of a slider, because a stroke is not a field patch
and the slider modes cannot drive one. It builds the recipe the figure is about — one brush mask
of one component, seeded by one stroke, and one masked Basic exposure layer, asserted in the captured
state rather than assumed, plus any requested Lens, Perspective and crop — and then paints one stroke
whose positions are handed to the desktop one
per 24 ms in real time: the first tick presses, each later one moves and the last releases, so one
paced step is still one stroke and one history entry. `--samples` is the number of positions, 2 to 1000.
Each position is its own mask `draft.set`, preview job and displayed frame, paired by the generation
`mask_draft_preview` carries, and a position whose job a later one superseded is reported as
`superseded` rather than averaged away. `latency.json` records the recipe it was painted on, the brush,
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
exact-query versus retained-readout counts, coalescing, photo writes and sampled RSS. Use
`--action set-basic --parameter exposure` for a masked +0.5 EV control; `--zoom`, `--crop` and
`--basic` exercise the same route in other views and stacks. Hover takes 1–240 scheduled inputs;
the emitted count is the distribution's sample count.

`--mode crop-start` measures opening a crop draft at Fit. It commits the recipe the flags ask for
(`--lens`, `--perspective`, `--crop`, `--basic`, and `--presence` for a Presence layer with all three fields at 100), opens the
Performance section, settles for 1.5 s, then per sample Starts a crop draft, holds it open for 1.5 s,
cancels it and settles 1.5 s more; `--samples` is 1 to 14 and `--zoom` is refused. The complete
script must remain within the 64-step evidence bound, including all preconditions; fourteen samples
with Lens, Perspective and crop use 63 steps. From events every binary logs it reads
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

On macOS, smoke, hardening, measurement, latency and RAW editor subprocesses always use the same background bundle as `develop --background`, and every one of them that launches the editor passes `--hidden-window`, so the run has neither an activated process nor a window on screen. Reports record `launch_mode`; reproduce through the harness to preserve focus protection. Smoke, `editor-latency` (every mode), `measure` and `hardening` make every editor launch through one envelope, `xtask/src/scenario/launch.rs`, which assembles each argument list, waits on each process through one watch loop (each tool keeps its own poll rate and deadline) and writes the run's `result.json` and `reproduce.md`: `result.json` lists every editor process the run started under `launches`, with its command and exit code, a launch the harness ends itself (an ordinary launch it has finished watching, such as an idle process) listed as `stopped`, and that list is what `verify` counts. Each tool's own report (`latency.json`, `measurements.json`, or `hardening`'s `result.json`) carries one provenance header: `launch_mode`, `platform` (the Rust host triple), `profile` (read from the `debug_assertions` the binary itself reports in its `startup` event), `binary_sha256` and `lockfile_sha256`. A native graphical session is still required. Windows and Linux retain direct launches; background behavior is not claimed there. Measurement launch times include the temporary bundle and executable copy, so they do not measure normal foreground activation, and with an invisible window they do not include the cost of compositing a visible one either.

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
4. When the change is complete, run `verify --tier quick` once. For UI or image changes, also run the smoke scenarios the change touches or the `rendered` tier, and inspect the captures as images.
   On macOS, use the background harness or `develop --background` for every automated GUI launch; use the live API and renderer readbacks to drive and inspect it. Only perform foreground interaction checks when the owner explicitly requests them.
5. For changes under `crates/`, answer the [performance rules](performance-rules.md) checklist and, once the feature is complete, run `editor-performance` on a generated 24 MP input in release, or the `timing` tier.
6. Report exact commands, artifact paths, results and unsupported cases. Update task and feature status only when acceptance is met.

## Packaging

`cargo xtask package` builds an unsigned host development artifact: a ZIP on macOS and Windows or a `.tar.gz` on Linux containing `Luxforge/` with the executable, notices, `build.json` (source revision, dirty state, target, profile, binary and lockfile hashes) and `checksums.txt`. Run smoke against the packaged executable with `--binary`. Packaging is repeatable, not byte-reproducible, and inventory is not a completed license audit. macOS bundles are unsigned and not notarized. No signing, stores or auto-update exist.

## CI

`.github/workflows/check.yml` runs the whole `cargo xtask check` (slow tests, doctests and the golden-fixture test included) and `editor-acceptance` in release, an optimized build and packaging on macOS arm64 and Ubuntu x64 with seven-day artifact retention, plus a separate dependency-policy job. Windows CI is disabled; Windows support will come later. Linux additionally runs eight smoke scenarios (`empty` through `large60`, the first eight of `smoke --list`) against the packaged binary under Xvfb with software Vulkan and records runtime imports. The list is written out in the workflow because `smoke --list` says which scenarios need a supplied RAW but not which run on software Vulkan; the other scenarios are not run in CI. Hosted results are compilation and functional evidence, never native desktop or GPU acceptance. Inspect actual run results for the tested commit; a configured step is not a passing result. Fresh hosted verification of the current tree and manual Windows/Linux desktop checks are open items on the [roadmap](../plan.md).

### Lens edge annotations

`lens-qualification` reads the existing RAW manifest reader and an annotation file with `{"format":1,"sources":[{"id":"manifest-id","edges":[[[x,y],...],...]}]}`. Coordinates are full-resolution content pixel centres before optional recipe geometry. Each marked photo needs at least three edges with at least five points each. A corrected edge must have at most 25% of the original maximum orthogonal line-fit deviation and at most 3 px after normalization to a 6,048-px long side. An empty `sources` array exercises selection and export for every manifest photo while reporting edge quality untested. It does not qualify missing photographs or fabricate edges. Outputs include the query, frozen selection, mapping descriptor, export result and unchanged source hash.
