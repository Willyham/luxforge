# Window visibility and event-driven job monitoring

Status: implemented and verified on the M4 Mac. Optimized process CPU observations are recorded with host-load scope; a generalized quiet-host baseline is not claimed. The owner limited this work to visibility and event monitoring. CPU observations are scoped in [performance](../specs/performance.md), without a literal zero-CPU claim.

## Outcome and scope

An editor window that cannot be seen stops resource sampling and display-only elapsed-time refreshes. Export and capability readers wait for relevant job changes rather than checking jobs every 100 ms. Work and its authoritative results continue while the window is hidden, and restoring the window shows current state without replaying a backlog of progress messages.

In scope:

- One desktop window-visibility state, updated by native window events, shared by the Performance sampler and display-only progress refreshes.
- Pausing resource sampling while the window is minimized or hidden, retaining the person's saved disclosure choice, and starting a fresh sample window on restore.
- Removing hidden-window elapsed-time refreshes and coalescing presentation-only progress while retaining lifecycle handling.
- Bounded, race-free notifications for the jobs the desktop actually follows, and a schema-described programmatic way to wait for those changes through the shared command service.
- Moving export and capability readers from periodic `job.read` calls to notification-driven reads.
- Native verification of visibility transitions, job completion while hidden, recovery on restore, and recorded CPU/wakeup observations.

Out of scope: proxy CPU work, renderer changes, preview/decode scheduling, speculative preview cancellation, shared-pool scheduling, image caches, whole-window redraw optimization, changing frameworks or graphics libraries, and changing the visible Performance section's one-second sampling cadence.

## Visibility contract

The owner selected minimized or explicitly hidden only. A visible window continues sampling when another app has focus or completely covers it; focus loss and occlusion alone never prove that presentation should pause.

Visibility is a desktop fact, not an editing or catalog rule. Keep it outside recipes and persisted catalog/session state. Each consumer uses the same fact rather than defining its own approximation. Native callbacks carry copied state into a bounded, coalescing wake channel; they do no catalog work. Registration, an initial state read, notification delivery and removal must avoid a lost transition. No visibility polling timer is added.

Iced 0.14 exposes minimized-state queries but no minimized/occluded event in its public window event vocabulary. On macOS, use a native observer through the existing safe platform boundary (`luxforge-input`) without replacing the window framework's delegate. Handle both window minimization/restoration and app/window hiding/showing. Use actual platform visibility, not `Unfocused`, zero dimensions or absence of frames. Verify Windows and Linux event semantics separately; report unsupported visibility facts explicitly rather than inventing a timer or silently equating focus with visibility. Automated evidence launches deliberately create invisible windows, so the evidence harness must distinguish a test override from an ordinary user's hidden window and state that distinction in its output.

On a transition to hidden:

- Drop the Performance sampler's timer and stop admitting new reads. A read already in flight may finish; discard its sample through the existing epoch check.
- Retain the Performance disclosure preference. Hidden state must never save a collapse or change the person's panel settings.
- Stop the once-per-second catalog progress refresh whose purpose is elapsed time and stalled-estimate presentation. Do not discard board/lifecycle notifications or the actions that follow job completion.
- Coalesce presentation-only progress to current state. Completion, failure, cancellation, required consent, source adoption, and job-dependent follow-up operations remain live.

On restore, reconcile current job/board state once, re-arm any needed display refresh, and restart Performance sampling only if its section and panel are shown. Clear resource-sample history and take a new sample immediately; CPU and GPU rates require a second sample and must not span hidden time. Do not replay every missed elapsed-time tick or progress value.

Exports, imports, indexing, original watching, explicit API requests, preference persistence and GPU resource retirement continue as today. Pausing presentation never cancels a job, changes an image or delays releasing retired resources.

## Job notification contract

Export and capability readers use `app/job_reads.rs`: they observe each job once and hold `job.wait` until its change token advances. Only first, changed and terminal records become UI messages. Each capability job has a stable subscription identity, so changing the set does not restart unrelated readers. A visibility transition restarts each affected reader once to reconcile current progress.

The activity board is not a complete job-notification service. Queued jobs need no activity, short activities may leave no recent entry, partial job results may change independently of progress, and some worker activity ends before the owner publishes the final job result. A wake that causes a read of `running` must not be the last wake before the owner stores `ready`.

Add notifications at the authoritative job lifecycle boundary. Cover queued/running transitions, observable progress and partial-result changes, every terminal outcome, and any removal or loss of access that changes what a reader can observe. Terminal notifications are issued only after `job.read` can observe the final result or error. A worker may signal progress without doing owner work; wake handlers only signal, never call the owner while holding its locks.

Registration and observing/re-arming must have an explicit change token or equivalent atomic protocol: a change between a read and the next wait must produce an immediate answer or buffered wake. A subscriber arriving after completion reads the final record immediately. Completion before subscription creation and completion after a quiet read are both required cases.

The registered method is `job.wait {job_id, after?, timeout_ms?}` → `{change, job}`. Without `after`, or for a terminal job, it answers immediately. An unchanged token waits indefinitely by default; an explicit timeout is bounded to 30 seconds, and zero means an immediate observation. Both the desktop's async route and JSON transport use the shared command service. At most 32 requests are held per owner and 16 per client; exhaustion returns `resource-limit`. Job retention uses the existing bounded lane tables. One buffered wake per observed job coalesces progress; tokens are monotonic u64 values, with exhaustion failing explicitly rather than wrapping. Future cancellation, transport EOF and disconnect remove held requests. No waiter thread or periodic clock exists for an indefinite wait. Keep existing `job.read` and `job.cancel` authoritative, including source/analysis ownership and globally readable lane jobs. Notification waiting must use an asynchronous or held-request path: never block the catalog owner, UI thread or async executor thread, and never add a thread per held waiter.

Name and enforce limits for waiters and watched jobs, pending wakes, change tokens, and retained records. Reuse established client limits where appropriate; exhaustion returns `resource-limit`, not an invisible fallback to polling. Disconnect, reader cancellation and job eviction remove interests safely. Coalesce wakes without losing a terminal record or allowing a client to read a job it does not own. Job updates stay separate from the catalog mutation log so progress cannot evict recipe/history events.

## Desktop integration

Export and capability readers first observe each tracked job, then sleep until it changes. Keep reads off the update loop and make one first, changed or final record available to the same existing adoption paths. Re-arm without a lost wake, and stop watching after a terminal answer or error. Changing the watched job set is bounded and does not restart unrelated readers unnecessarily.

While hidden, lifecycle results are still adopted promptly. Display-only intermediate progress can be coalesced, but no held notification may suppress the final result or dependent work. The catalog activity watch supplies presentation progress and lifecycle display. Stable authoritative readers independently follow first-look Reading, Adding, Search, Locate and Batch jobs; Search retains partial business answers while coalescing hidden progress. Develop already waits for the owner's final publication through its existing completion route. The elapsed-time refresh gate is separate from lifecycle processing. All visibility-driven presentation reconciliation uses the common visibility state.

## Acceptance and evidence

The core job-monitoring contract is also qualified by focused tests in the owner's ARM64 Omarchy VM (Arch Linux, kernel 7.2.6, Rust 1.94.0): bounded waits and cancellation, socket disconnect cleanup and response ordering, and progress/terminal publication. The portable visibility model passes its minimize/hide-only gate test there. These are functional tests, not native Linux event, desktop, GPU or idle-CPU evidence. Linux still reports native visibility facts as unsupported and retains normal UI sampling when a window is minimized.

- With the section open, minimized/hidden windows admit no further resource reads after any in-flight read completes, and create no resource or elapsed-time display timers. Visible unfocused windows retain normal behavior.
- Restore retains disclosure, reconciles current jobs once and begins a fresh resource history without a rate computed across hidden time.
- Unchanged queued/running export or capability jobs trigger no periodic `job.read` calls. Progress/partial changes and terminal outcomes wake the correct reader with no polling, lost completion or unbounded queue.
- Jobs can complete, fail or be cancelled while hidden; their owner state, outputs and dependent operations match visible operation. Original files and recipes are unaffected.
- Tests force races around registration, reading/re-arming and result publication. Cover queued cancellation, very short jobs, partial results, disconnect, watcher exhaustion, ownership and eviction.
- Real background-only native checks correlate visibility callbacks, subscription state, read counts, job state and captured restored frames. Synthetic visibility messages alone do not prove native minimization or hiding. Do not activate the editor or the owner's existing instance.
- Run the quick tier and the touched rendered scenarios. Record optimized M4 process CPU observations with sampling expanded/collapsed, visible/hidden idle states and held jobs. Record host load, process CPU time, application wake/read/update counts, exact windows, observer overhead and build identity; a quiet-host baseline or speedup needs separate evidence when host load prevents it. Claim no literal zero CPU. Windows/Linux results remain separately scoped.

## Owner decisions

The owner limited scope to visibility and event monitoring, excluding proxy CPU work and CPU scheduling, and selected pausing only when minimized or explicitly hidden. Fully covered and unfocused windows retain their normal sampling behavior. There are no outstanding product decisions in this scope; native event and notification mechanisms remain implementation choices to verify.

## Performance-rules review

No original reads, decodes, frame allocations, image processing or new preview requests are introduced. The owner handles bounded job bookkeeping only. Notifications and visibility events replace idle polling; active display throttles exist only while a visible consumer needs them. All new queues/subscribers have explicit limits, and notification callbacks never read under producer locks. Rendering, image correctness, caching and CPU scheduling remain outside this work. Native CPU evidence is recorded after implementation, with process monitoring overhead and hidden-window evidence controls stated.
