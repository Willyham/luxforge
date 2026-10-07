# Performance panel, activity board and resource counters

Status: implemented on the M4 Mac, including the owner's 2026-10-02 request for saved disclosure, cancellation and lower redraw cost, whose [decisions](#decisions) on its default state and units are recorded below. It adds a Performance section at the bottom of the state panel that shows the editor's own memory, CPU and GPU use with a one-minute sparkline each, and the long-running work in progress. Behind it are two host services with their own API methods: an **activity board** that any worker publishes its work to and any client reads, and **resource counters** that report what the operating system accounts to this process. Neither changes a recipe, history, render or export.

## Why a host inspector and a board, not a tool module and a bus

A tool module in Luxforge is an editing provider: a descriptor, effects, actions and layers ([modules](modules-and-api.md)). A resource monitor has none of those, so it is a host inspector like the histogram, and its data is reachable through ordinary API methods, which is what makes it programmable.

The coupling the owner wants to avoid is between the panel and the things that do work. The panel must not know the preview worker, the source worker, a later exporter or an AI provider, and none of them may know the panel. The **activity board** is the seam: a publisher calls the host's `begin` and gets a guard; a reader takes a snapshot. Publishers and readers share the board's schema and nothing else.

The board holds current state rather than delivering a stream of events, for three reasons:

- A reader that arrives late (the panel opened mid-export, an agent that connects during a RAW redevelopment) needs to know what is running now, not the transitions it missed.
- The existing event log (`events.since`) is a 256-entry record of catalog mutations that clients use to resynchronise. Preview jobs start and finish up to 120 times a second during a drag; routing them through that log would evict the mutations and force full refreshes.
- A snapshot costs one lock and a copy of at most 64 small entries, with no subscriber bookkeeping and no queue to bound per reader.

The board's `sequence` changes whenever its contents change, so a poller can skip an unchanged snapshot. A reader in the host's process can watch the board instead of polling it (`ActivityBoard::watch`), which the desktop's long-running work does ([below](#long-running-catalog-work)); the section's sampler reads the board through that same watch, so the section and long-running work show one read of it.

## Scope

In scope: the activity board and `activity.list`; the resource counters and `resources.read`; publishing from the work that exists today (source preparation, RAW redevelopment, preview jobs, owner-side analysis and capability jobs); the Performance section with its sampler; widgets, evidence and measurement.

Out of scope: AI jobs, which will publish through the same board when they exist; per-cache memory attribution; and GPU time and GPU allocations on Windows and Linux. Exports publish through the existing board. The section remembers its disclosure state and cancels jobs through their existing `job.cancel` operation.

## Activity board

`luxforge_core::activity`. One board per catalog owner, created at `OwnerHandle::start_with` and shared as `Arc<ActivityBoard>` with the owner's workers; `OwnerHandle::activity()` hands the desktop the same board for its preview queue.

- `board.begin(spec) -> Activity` records an active entry and returns a guard. `spec` is `{kind, label, detail?, asset_id?, job_id?}`: `kind` is a stable dotted identifier, `label` a short present-participle phrase for people, `detail` an optional second line (a file name), `job_id` the id of a job that `job.read` can also answer, when there is one.
- `activity.phase(&'static str)` and `activity.progress(fraction, message)` update the entry: `fraction` is 0 to 1 when the work knows a truthful extent, `message` a short word for what it is doing (`downloading`, `copying`). Either may be omitted. A capability job's `ModuleContext::progress` is this call, forwarded through its `JobControl`; `job.read` answers with the same progress rather than keeping its own copy.
- `activity.finish(outcome)` with `completed`, `cancelled` or `failed` moves the entry to the recent list. Dropping the guard without finishing records `cancelled`, or `failed` when the thread is panicking, so an entry can never outlive its work.
- Bounds: at most 64 active entries; a `begin` past that is still allowed to run, returns a guard that records nothing and increments `untracked`. At most 16 recent entries, newest first, and only work that ran for at least 250 ms enters the recent list, so a drag's preview churn never evicts a RAW redevelopment. The threshold is a board constant that tests may lower.
- Cost: `begin`, `phase` and `finish` each take one uncontended mutex and allocate only the optional detail string. Labels are `&'static str`. No timer, thread or queue is added.
- Watches: `board.watch(follows, wake)` registers a reader, at most 8 per board (`resource-limit` past that). `follows` picks the entries it cares about; once the watch has read the board (`ActivityWatch::read`, the snapshot and the re-arming under the one lock), the next begin, phase, progress or end of such an entry calls `wake` once, on the publishing thread, and the watch is not woken again until it reads again. So a watcher is woken at most once per read, never while nothing it follows changes, and no change can fall between a read and the next wake. `wake` only posts a signal. Dropping the watch removes it.

Publishers today:

| Kind | Label | Published by | Detail | Phases |
| --- | --- | --- | --- | --- |
| `source.prepare` | Preparing original | Source worker, `SourceTaskKind::File`, from before the owner marks the job preparing to before it learns the result | File name | none |
| `source.develop` | Developing RAW | Source worker, `SourceTaskKind::Develop` | File name | none |
| `preview.render` | Rendering preview | The preview queue's job thread, from its start to the end of its one phase, before its result is sent | none | `proxy` for a CPU proxy job (a drag in a session without a GPU), `exact` for a reference frame |
| `analysis.histogram` | Measuring histogram | The owner's analysis job thread, until before its result is posted | none | none |
| `gpu.warm` | Preparing GPU renderer | The desktop, on the owner's board (`ActivityBoard::begin`), for a warm-up of the photo surface's compile thread: from the update that first sees it running to the one that sees its queue drained ([GPU previews](gpu-preview.md#warming-at-launch-and-open)) | none | none |
| `module.resource.install` | Installing resource | The capability worker's transfer lane | `<module ID>/<resource ID>` | none |
| `module.resource.remove` | Removing resource | The capability worker's transfer lane | `<module ID>/<resource ID>` | none |
| `module.task` | Running task | The capability worker's module lane | Module ID | none |

Every entry but `source.prepare` names its `asset_id`; that entry names the file it reads. Capability jobs name no `asset_id`: a task's photo is in its own request, not the board's schema. An entry's `job_id` is the job `job.read` answers, whatever its kind. Each entry ends before its result reaches a reader, so a client that sees the job finished never still finds it running. A cancelled source job, a superseded or abandoned exact phase and `ErrorKind::Cancelled` end `cancelled`; any other error ends `failed`.

The clipping overlay and preset import are left out: the first is a desktop-local reduction measured in milliseconds, the second runs synchronously on the owner.

### `activity.list`

No parameters (an omitted or empty `params` is accepted and any key is refused, so a misspelt filter is never ignored), no asset required, mutates nothing, emits no event. The owner answers it from its board.

```json
{
  "sequence": 812,
  "active": [
    {"id": 41, "kind": "source.develop", "label": "Developing RAW", "detail": "DSC_0412.NEF",
     "asset_id": "…", "job_id": "job-…", "elapsed_ms": 1204}
  ],
  "recent": [
    {"id": 40, "kind": "preview.render", "label": "Rendering preview", "phase": "exact",
     "outcome": "completed", "duration_ms": 1610, "ended_ms_ago": 4020}
  ],
  "untracked": 0
}
```

Optional keys (`detail`, `asset_id`, `job_id`, `phase`, `progress` as `{"fraction"?, "message"?}`) are omitted when absent. Active entries are oldest first. Times are whole milliseconds computed from monotonic clocks at snapshot time.

## Resource counters

A new leaf crate, `crates/luxforge-process`, holds the platform code. It is the second crate allowed `unsafe` (as `luxforge-raw` is, with `unsafe_op_in_unsafe_fn = "deny"`), keeps every `unsafe` block beside a `SAFETY:` comment and exposes a safe API. It adds no crate that is not already in `Cargo.lock`; direct dependencies are pinned to the locked versions.

| Counter | macOS (owner's M4) | Linux | Windows |
| --- | --- | --- | --- |
| CPU time, all threads, user + system | `proc_pid_rusage` v6, mach ticks converted with `mach_timebase_info` | `/proc/self/stat` `utime + stime` over `sysconf(_SC_CLK_TCK)` | `GetProcessTimes` |
| Memory | `ri_phys_footprint`, kind `footprint`: Activity Monitor's Memory column, which on unified memory includes GPU allocations | `VmRSS`, kind `resident` | `PrivateUsage`, kind `private` |
| Peak memory | `ri_lifetime_max_phys_footprint` | `VmHWM` | `PeakWorkingSetSize` (resident) |
| Resident memory | `ri_resident_size` | `VmRSS` | `WorkingSetSize` |
| GPU time | Sum of `accumulatedGPUTime` (ns) over this pid's `IOAccelerator` user clients in the IORegistry | unavailable | unavailable |
| GPU allocations | `currentAllocatedSize` of the system default Metal device, which is the process-wide device wgpu also uses | unavailable | unavailable |

Two facts were measured on the M4 on 2026-09-23 with a Metal compute probe: `task_power_info_v2.gpu_energy.task_gpu_utilisation` stays 0 on Apple silicon and cannot be used; the IORegistry `AppUsage` entries report nanoseconds (0.80 s against 0.82 s of command-buffer time), and `MTLCreateSystemDefaultDevice` returns the one device object whose `currentAllocatedSize` counts the process's allocations. `AppUsage` is an undocumented key the driver publishes; if it is missing the counter reports unavailable with that reason, never zero. Walking the accelerator's children costs 0.35 ms p50 and 1.1 ms p95 (84 user clients on the owner's machine), so the sampler keeps this process's user-client entries and asks only them on later reads, walking again every 10 s, whether or not it found any, and sooner when a cached client stops answering. Releasing a command queue removes its entry from `AppUsage`, so the sampler folds a vanished entry's last value into a retired total and GPU time never decreases.

The counters belong to the process, not to a catalog owner, so `luxforge_core::resources` keeps one process-wide sampler; the budgets it reports beside them are the catalog owner's render context's. Reading the Metal device creates one in a process that has none, so GPU allocations are read only after the host calls `luxforge_core::resources::declare_gpu_presenter()`: the desktop does at startup, before its first frame; the headless `luxforge-json` owner does not, and reports the reason.

### `resources.read`

No parameters (refused like `activity.list`'s), no asset required, mutates nothing, emits no event. Counters are cumulative, so a client computes a rate from two reads: CPU percent of one core is `100 × Δcpu.time_ns / Δmonotonic_ns`, as Activity Monitor counts it (a 14-core machine can reach 1400%), and GPU percent is the same over `gpu.time_ns`.

```json
{
  "monotonic_ns": 81234567000,
  "cpu": {"time_ns": 5231200000, "logical_cpus": 14},
  "memory": {"kind": "footprint", "bytes": 1523000000, "peak_bytes": 2011000000, "resident_bytes": 980000000},
  "gpu": {"time_ns": 812400000, "allocated_bytes": 312000000, "unified_memory": true},
  "budgets": {
    "colour_scratch": {"target_bytes": 67108864, "in_use_bytes": 0, "peak_bytes": 3145728},
    "spatial": {"target_bytes": 268435456, "in_use_bytes": 0, "peak_bytes": 50331648}
  }
}
```

`monotonic_ns` is only meaningful as a difference. A counter the platform cannot give is omitted, and its object gains `"unavailable": {"<key>": "<reason>"}`, for example `"gpu": {"unavailable": {"time_ns": "GPU time is not reported on Linux yet", "allocated_bytes": "…"}}`. The two budgets are the owner's render context's colour-scratch and spatial targets with their high-water marks, which every preview and analysis render shares; they are exact and cost nothing to read.

`unified_memory` is omitted, with no reason, when no device was read, since it is a property rather than a counter. A read runs on the owner thread: a handful of system calls plus, with the entry cache warm, a few IORegistry property reads, 4 to 8 µs in all ([verification](#verification)). It is bookkeeping, not frame work (performance rule 5). The first read after `declare_gpu_presenter()` opens the Metal device handle: 0.6 ms in the desktop, whose device already exists, and 37 ms in a process that has none.

## Performance section

The shared [visibility gate](visibility-and-monitoring.md) pauses sampling on macOS when the editor is minimized or the window/app is explicitly hidden. Unfocused and covered visible windows keep sampling. Windows/Linux native visibility facts are currently unsupported; the evidence state reports that limitation explicitly. Export and capability readers use authoritative `job.wait` notifications without a polling interval.

The last block of the state panel, outside its scrollable so it stays pinned to the bottom while History scrolls above it; a 1 px rule in the band-border colour separates the two. It is open on first use, remembers its disclosure state, and **samples only while it is expanded, the state panel is shown and native visibility permits presentation** (the developer components gallery, which replaces the workspace, counts as hidden): collapsed, it sets no timer and makes no request, so idle stays asleep (performance rule 8). Expanded, it reads `resources.read` once a second through one owner task and, at the same tick, the activity board through the desktop's watch on it (`ActivityWatch::read`: what `activity.list` answers, on the update loop, without an owner round trip), skips a tick while the previous read is in flight, and keeps the last 60 samples; expanding or restoring clears the history and reads at once. A hidden transition invalidates an in-flight sample without cancelling its read; CPU/GPU rates never span the hidden interval. It parses the counters into the core's own report type (`ResourceReport`, which deserializes from the JSON it serializes to), so the section has no copy of its schema, and draws its job rows from the board as the desktop last read it (`ActivitySnapshot`), the snapshot long-running work holds: every read long-running work takes when the board wakes it refreshes the rows too, so they follow catalog work between samples and never disagree with the status bar's job ([below](#long-running-catalog-work)). With no watch (the board refuses more than eight) there is no board to read, and the section lists no work, as long-running work shows none. The current expanded state belongs to this client. Toggling it saves the next-launch default in the host's `preferences.json`, outside any catalog. `preferences.read` returns `{performance_expanded}` and `preferences.set {performance_expanded}` changes that default through the same bounded atomic writer used for module settings. A read creates nothing; malformed or unsupported settings fail explicitly and remain untouched. Writes are coalesced, and a normal window close waits for the newest offered preference to be saved before stopping the owner. A command client can set the next-launch default without changing an already-open window. Show performance and Hide performance in the command palette toggle it, and the evidence step `{"performance": {"expanded": …}}` drives it in scripted runs ([development](../engineering/development.md)).

![Performance section mockup](performance-panel/mockup.png)

Layout at the panel's 240 pt, inside its 8 pt padding:

| Element | Size | Rule |
| --- | --- | --- |
| Heading | 22 pt | `PERFORMANCE` section label; right-aligned caption (`2 jobs` while long work is active, else empty) and a 10 pt chevron at the far right, down when expanded; the whole row toggles |
| Metric row | 24 pt, three rows: Memory, CPU, GPU | Label, 11 pt secondary in a 48 pt box; the sparkline filling the middle, 16 pt tall; the value, 12 pt primary right-aligned in a 56 pt box with its unit in 10.5 pt tertiary; 8 pt gaps |
| Sparkline | 60 samples, newest at the right edge | 1 px baseline `#313134`; area at 16% of `#a3a3aa`; 1.25 px line `#a3a3aa`; the newest point a 1.75 pt dot `#ececee`. No accent and no colour: the photograph is the only colour on screen. Until 60 samples exist the line starts part-way across |
| Job row | 16 pt label line, 14 pt detail line, optional 2 pt bar | A 6 pt marker (filled `#e8e8ea` running, hollow tertiary finished), the label in 12 pt primary, elapsed on the right in 10.5 pt secondary; the detail line in 10.5 pt tertiary aligned with the label, reading the entry's detail and its phase joined by ` · ` (`DSC_0412.NEF`, `exact phase`), and omitted when it has neither; a determinate progress bar under it only when the entry has progress |
| Work row | Label line, then a bar and count line | Running catalog work, drawn as the [catalog components board](catalog/components.png) draws it (`luxforge_ui::work_row`): the accent dot, the label with the place or file the work acts on, the estimate once one is truthful and **Cancel** on the right; under them, indented, a bar filling the width, filled only with the fraction the work reports, and the work's own count, or `working` |
| Bottom | 8 pt | Panel padding |

Values and scales, all derived by pure functions in the view model:

- **Memory** shows `memory.bytes` in binary units with Activity Monitor's labels: `812 MB` below 1 GiB, `1.42 GB` above, `12.4 GB` from 10 GiB. Scale 0 to 110% of the window's largest sample, at least 64 MiB. Tooltip: the kind in words (Activity Monitor's Memory for `footprint`), the peak since launch, resident memory and, on unified memory, the GPU allocations it includes.
- **CPU** shows percent of one core: one decimal below 10% (`3.2%`), whole numbers above (`38%`, `420%`). Scale 0 to the larger of 100% and the window's peak. Tooltip: the convention in words with the machine's ceiling (`14 cores: 1400%`) and the window's peak.
- **GPU** shows percent of GPU time the same way, scale 0 to 100%. Tooltip: the window's peak and the GPU allocations. When a counter is unavailable the row shows `–`, draws only its baseline and its tooltip gives the reason.
- CPU and GPU need two samples; the first second after expanding shows `–` for both.
- **Jobs**: a running row with a valid `job_id` offers a compact Cancel button. Its request is the ordinary `job.cancel`; the pending request disables a duplicate, and success, an already-finished job or a refusal is reported in the status bar. Source and analysis cancellation release this client's interest; capability and export cancellation stop the shared job. Preview work without a job ID has no Cancel button. Every active entry that has run for at least 500 ms, oldest first, at most four and then a `+N more` caption; when there is none, the newest recent entry that ran for at least 500 ms and ended within the last 10 s, dimmed with a hollow marker, its duration on the right and `Finished 4 s ago` (or `Cancelled`, `Failed`) as its detail; otherwise one dimmed `No background work` row. There is always at least one job line, and a job line without a detail keeps an empty detail line beneath it, so the section's height changes only when two or more long jobs overlap. Elapsed reads `0.8 s`, `12 s`, `1 min 4 s`.

At one sample a second, a job shorter than about a second may never be seen running; it is still in `activity.list` and appears here as finished. The 500 ms display threshold is a view rule; the API reports every active entry.

### Long-running catalog work

The catalog's jobs — indexing (`index.refresh`), reading and rendering previews, the 100% region, developing picks, checking, finding and verifying originals, batch preset and export, each an activity board kind named in `catalog_types::jobs` — are rows that can be stopped ([catalog design](catalog.md#long-running-work)). Each running one is a work row: its label names what it works on (`Indexing ~/Pictures`, `Indexing the NIKON Z 8 card`, `Rendering previews · DSC_0412.NEF`; a path under the home folder from `~`, one longer than 28 characters by its last component), its count is the work's own progress message, its bar its reported fraction and nothing else, and **Cancel** sends `job.cancel` with the entry's `job_id`. A finished catalog row keeps that label, and its detail says how it ended (`Cancelled 1 s ago`). The desktop's own work — preparations, RAW development, renders, histograms — keeps the plain job row, which offers Cancel only when its entry has a `job_id`, as the Jobs rule above describes.

- **Estimate.** Shown only once the rate is steady, from the fraction the work reports at each read of the board: at least 4 updates over at least 2 s within the last 10 s, with the rate over the newest 4 updates within 25% of the rate over the whole window. It stays while that rate is within 50% of the window's and is withdrawn past it, when progress goes back (a new phase measures from its own start), when the work stops stating a fraction, when it stalls — nothing has moved for three times its usual interval between updates and at least 2 s — or when the time left at that rate has run out. It reads `about 4 s`, `about 1 min 40 s` (tens of seconds under ten minutes), `about 14 min` or `about 1 h 20 min`, never `about 0 s`. The rules are `state/long_work.rs`'s, tested with made-up boards.
- **Truthful counts.** The index lane reports a count while its walk is still finding a folder's files (`5,120 files found`, or `48,210 of about 200,000 files` from an earlier listing) and a fraction only once the walk has listed everything and so knows how many headers there are to read, so a first look never reads nearly done while most of the folder is still unlisted.
- **Waking.** These rows, the status bar's job, the in-view progress sheet and the finished sentence all come from one read of the board, which the desktop takes through its watch on it: the section's sampler at its own tick, and long-running work whenever the watch, which follows catalog kinds only, wakes it — at once unless the board was read in the last 250 ms, when a timer that exists only while a read is due reads it 250 ms later; while a catalog job runs the board is also read once a second, since the display threshold and a stall can only be seen as time passes. So a catalog job's row appears, moves and ends between the section's samples, as the status bar's job does, and the two never disagree. With no catalog job running long-running work reads nothing and holds no timer, whether the section is open or not; the section's own tick is its only read then, and only while it samples.

## Performance rules checklist

- **Original reads and decodes:** none added.
- **Full-frame allocations:** none. The board holds at most 80 small entries; the desktop keeps 60 samples of a few integers.
- **Point queries:** none added.
- **Owner thread:** `resources.read` (system calls and IORegistry reads, measured) and `activity.list` (one lock and a copy). No frame work.
- **Desktop messages:** a tick every second while the section is expanded, with one owner task (`resources.read`) and one read of the board on the update loop per tick. An idle Performance-only tick or answer refreshes only the section; active gestures, workers, view changes and evidence use the ordinary full derivation. No `asset.state`, `history.list`, preview job or pixel upload is added. Unchanged photo surfaces skip tile-layout allocation and uniform writes during redraw.
- **Timers:** one, gated on the section being expanded, the state panel shown and shared presentation visibility; its interval is the sparkline's resolution. Its cost is measured as idle CPU with the section expanded and collapsed. Long-running catalog work adds no polling: its watch wakes it, and its two timers exist only while presentation is visible and a read is due or a catalog job runs.
- **Timing:** `editor-latency` Exposure drag before and after, since every preview job now begins and finishes one activity.

Hidden windows disable both long-work display timers and presentation-only board progress wakes. Begin/end lifecycle notifications remain active. Authoritative job waits retain Select first-look, folder-add and Locate results, including partial search answers, independently of worker activity ending. Restore reconciles the board once and shows current progress without replaying intermediate values.

## Acceptance

- Core: board unit tests for begin, phase, finish, drop-as-cancelled, panic-as-failed, the 64-entry cap with `untracked`, the recent threshold and cap, and `sequence` changing exactly when contents change; an owner test that a source preparation and an analysis job appear in `activity.list` while running (held by a test gate, not a sleep) and in `recent` afterwards; a preview-queue test that a moving job's activity reports `proxy`, a job at rest's `exact`, and each ends when the queue releases it. Both methods are in `schema.list` with notes, need no asset and emit no event.
- Counters: tests that CPU time grows after spinning a thread for 200 ms by at least 150 ms; that footprint (or resident memory) grows by at least 48 MiB after touching a 64 MiB allocation; that peak is at least current; on macOS, that a `wgpu` or Metal dispatch raises GPU time and GPU allocations when a Metal device exists, and that the headless configuration reports allocations unavailable with a reason. Linux and Windows code compiles in the hosted CI matrix; no native run is claimed.
- Desktop: view-model tests for rates from counter pairs, every formatter's boundaries, the scale rules, the job display rules and the unavailable row; the section's timer exists only while expanded and the state panel is shown; widgets have pure geometry tests (sparkline points, clamping of non-finite and out-of-range values, an empty series) and gallery states.
- Rendered: a `performance` smoke scenario on the owner's M4 that captures the collapsed heading, the expanded section after at least three samples, a finished long job and the collapsed section again, and checks each frame's recorded samples against the displayed text, the rates against the recorded counters, the job rows against the recorded `activity.list`, the sample count unchanged while collapsed, and the memory figure against an independent reading of the same process taken by the runner.
- Measured and recorded in [performance](../specs/performance.md): the owner-side cost of each method, idle CPU with the section collapsed and expanded, and the Exposure drag before and after.

## Verification

On the owner's M4 Mac, release builds; panel polish qualified on 2026-10-02:

- `cargo xtask check` passes: the board's unit tests (active to recent with its last phase, drop as cancelled, panic as failed, the 64-entry cap and `untracked`, the threshold and the 16-entry cap, `sequence` changing exactly with the contents, the JSON shape), an owner test that holds a source preparation and an analysis job at test gates while `activity.list` lists them and then reads them as recent, preview-queue tests for `proxy` and `exact` jobs and for a superseded job ending `cancelled`, the counters' tests (CPU time grows by at least 150 ms over a 200 ms spin and falls within `getrusage`'s readings; footprint grows by at least 48 MiB after touching 64 MiB; a 256 MiB Metal buffer raises GPU allocations by at least its size; a blit raises GPU time to within 4× of Metal's own command-buffer time, and releasing the queue never lowers it; a headless process reports both GPU counters unavailable with their reasons), both methods through the method table and the owner, and the desktop's view-model, gate and sampler tests.
- Native mask, viewport and Performance checks correlate rendered frames, state and logs; the `capabilities` scenario cancels a real held task through its Performance row and checks the cancelled job and unchanged accepted edit. `smoke --scenario performance` opens the generated 60 MP JPEG and captures twelve frames: opened with the section open and sampling, after 3.6 s, a 16:9 straighten at 3°, moderate Detail, Presence Clarity, Texture and Dehaze at 100 over them (each drawn by the GPU, which lists no job), the stack exported through the reference renderer in the background (about 1.8 s at 60 MP on the M4, 1.4 s without Detail), captured on the first read that lists it running as "Exporting JPEG · heavy.jpg · rendering phase" past the section's 0.5 s threshold, three frames 5 s apart, the first of which lists it finished and the last of which finds nothing running, collapsed, 2.5 s later with the reads and samples unchanged, and opened again on the first read of a fresh window. The runner re-derives every displayed figure, series length and job row from the recorded `resources.read` and `activity.list` answers without the editor's code, and requires footprint memory with GPU time and unified GPU allocations. It holds a settled frame's resident memory and footprint to its own `ps` and `footprint` readings of the same process taken just before and after the sample, where they agree to the byte: a wait step's frame whose board has nothing running, the filled window and the export's last frame among them. While the GPU draws the 60 MP stack at rest or the reference exports it, the footprint moves by tens to hundreds of megabytes inside one of the runner's intervals, so those frames' figures are recorded beside the readings, not gated. The running and finished rows must be the export's own entry, begun after every entry the frame before it recorded, so a long job from before it, such as the open's "Preparing original", cannot stand in, and its file must be written. With `--source` a RAW, the edits are the same over the straighten.
- Timing, idle CPU and each method's cost are in [performance](../specs/performance.md#performance-section-activity-board-and-resource-counters): idle model updates avoid unrelated workspace derivation, and unchanged tiles avoid layout allocation and uniform writes. Whole-window idle CPU varies between 0.83% and 1.19% of one core in the optimized 24 MP masked runs; an aggregate reduction is not established. Collapsed or hidden the sampler makes no reads; each owner method answers in a few microseconds.

Catalog work rows: the board's watch is unit-tested in the core (woken once per read by followed entries only, never after it is dropped, bounded), the rows, estimates and Cancel in `state/long_work` and `app/long_work` tests, the section's rows and the status bar's job drawn from one read after every wake, with nothing new read or timed while idle, in `state` and `app/long_work` tests (`the_section_and_the_status_bar_show_the_same_board_read`, `the_section_and_the_status_bar_never_disagree_after_a_wake`), and the `select` smoke scenario captures a first look's work row with Cancel after Continue in background and the job cancelled from it, its row then reading `Cancelled 1 s ago`, each against the recorded board.

Not verified: native Linux and Windows runs (their counters compile, and the unavailable rows are unit-tested); the `+N more` caption and the tooltips in a rendered frame (unit-tested); the heading's hover, which no scripted step moves the pointer over; a work row's estimate in a rendered frame (a first look reports no fraction until its walk has ended; unit-tested).

## Decisions

Decided by the owner on 2026-09-23, after reviewing the built section:

- **Default state.** The section starts open on first use and remembers the last disclosure choice (the owner's Performance panel polish request of 2026-10-02). Its one-second sampler counts in the ordinary session's [idle figure](../specs/performance.md#performance-section-activity-board-and-resource-counters) when expanded and visible; collapsing it or hiding the state panel stops it.
- **Units.** Binary sizes with Activity Monitor's MB and GB labels, and CPU as a percentage of one core, so it passes 100% whenever more than one core is busy.

## Outstanding inspection work

- **GPU memory attribution** (owner, 2026-10-07): add attribution to both `resources.read` and the Performance panel. This extension is not implemented. Today the API reports process counters and the reference renderer's colour-scratch and spatial targets. The owner holds prepared sources and RAW developments, and the CPU proxy in a session without a GPU. The desktop holds the GPU source, each slot's chain, intermediates and scratch pool, picture-at-rest tiles and photo slots under the 1 GiB ceiling, plus the tile worker's separate 2 GiB budget; the preview budget is also 2 GiB. The gpu-free core needs explicit host-reported charges, with unavailable reasons for a host that supplies none. [GPU memory accounting](../../tasks/rendering/gpu-memory.json) still has to qualify resources outside these budgets, including staging, crop/overlay textures and device/pipeline overhead; do not sum GPU allocations and unified-memory footprint as separate resident memory.
- GPU time and allocations on Linux and Windows, with native counter checks. The current unavailable reasons remain truthful.
- Reduce the expanded section's whole-window redraw cost. Idle model updates and unchanged surface writes cost less, but the recorded whole-window CPU observations do not establish an aggregate improvement.
- A correlated rendered frame of RAW development while it runs. Running reference export is already captured; it is a different workload.
