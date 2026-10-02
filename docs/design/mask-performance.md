# Mask performance and Performance panel polish

Status: implemented with scoped native M4 qualification, authorized by the owner on 2026-10-02.

## Outcome

Reduce the cost and latency of continuous mask interaction without changing the frozen coverage
equations, accepted draft identities, source pixels or one-entry-per-stroke history. Make the
Performance section cheaper while open, remember its disclosure state in user settings, and
let a person cancel a listed job through the same `job.cancel` operation an API client uses.

## Scope and constraints

- Diagnose the current native paced brush path at Fit and 100% on 24 MP and 60 MP inputs. Include
  photograph adoption, authoritative coverage readiness, superseded inputs, worker phases and
  process resources; latency claims distinguish adoption from GPU completion and scanout.
- Optimize repeated mask compilation/evaluation, scheduling, coverage presentation and desktop
  work where evidence identifies cost. Preserve exact coverage and the current progressive
  feedback/cancellation fences. Use existing bounded workers, buffers and caches; any additional
  image-sized retention requires a separately recorded budget decision.
- Keep unchanged photograph and overlay resources on the GPU rather than rewriting them on a
  view-only redraw. Avoid unrelated workspace derivation on a Performance-only sample.
- Persist only the section's disclosure preference outside the catalog, with typed command
  discovery and explicit errors for malformed settings. First use remains expanded.
- Offer Cancel only for running rows whose job provides an ID. Jobs without a cancel operation
  remain informative. A pending cancellation disables a duplicate request; completion and
  refusal follow the existing job semantics and status-message rules.
- Continue sampling at one-second resolution only while expanded and visible. Do not add an
  idle poll, change the metric units, relax numerical tolerances or hide slow/missing samples.

Density, mask presets, new selection kinds, GPU-evaluated editing, total-editor memory
qualification and other platforms' GPU counters remain separate work.

## Acceptance

Exact mask/reference, cancellation, draft and worker-bound tests pass. Native background
mask scenarios correlate captures, coverage and photograph identities. Repeated matched native
release measurements show a useful improvement in a diagnosed mask cost without worse long-stroke
progress, correctness or memory. Report workload, sample counts, host load and limitations.
The panel cancels a real supported job, remembers its state across launches through the command
service, remains asleep when hidden/collapsed, and has measured lower open-section work.
Finish with quick verification and affected rendered checks; record the performance-rules
checklist and current behavior in the feature status and user guide.

## Implementation and resource review

The brush index holds contiguous cell slices with the original segment and stroke order. Coverage
skips only the renderer's exact-zero support rectangle. The frozen mask equations, divisors,
coverage quantization, density and decimation remain unchanged.

Coverage painting uses an exact 256-code palette on the existing coverage worker. The worker keeps
one cached coverage plane; completions retain identity, dimensions and shared RGBA only. A blocking
paint slot admits one worker-owned RGBA result, including a pass still painting, and wakes on semantic
cancellation or queue destruction. The consumer holds the slot through adoption or replacement of
its previous waiting result, then releases it; a retained waiting result owns no lease. At most two
RGBA grids belong to the handoff, including the transient old/new consumer pair: eight bytes per
cell plus the one-byte coverage cache, at most nine bytes per cell. This stays within the former
handoff's maximum, which included UI RGBA conversion and separate cached, queued, worker and
consumer coverage copies. The presenter's existing Fit and region frames and
GPU slots retain their own bounds. No image cache, source read, point raster,
thread, timer or scratch-budget increase is introduced. Existing coverage-cell and input-grid limits
remain in force. Total editor memory remains a separate qualification.

The owner only handles bounded preferences and the existing cancellation operation. The desktop
coalesces preference saves, waits for the last save on normal close, caps pending cancels at four,
and refreshes only the Performance model for otherwise idle samples. Active work and evidence keep
the ordinary full update path. Unchanged GPU tiles keep their pixels, layouts and uniforms; changed
placement and frame identities still update through the same presenter.

A coverage result that precedes its matching photograph does not wake an undrawable redraw.
The desktop publishes adopted photo content before draining coverage in its existing after-message
hook. A completion already queued is found by that drain; a later completion sees the published
content and wakes normally. Unbound coverage on unchanged pixels and unavailable outcomes still
wake immediately. This adds only two atomic content stamps, no timer, runtime hop or pixel buffer.
The wake/publication, progressive feedback and failed-photograph tests guard liveness.

Performance-rules review for the finished change:

| Question | Answer |
| --- | --- |
| Original reads, hashing and decoding | No changed request path reads original payloads. The existing verified source cache and worker remain the only preparation path. |
| Image-sized allocations and sharing | The existing RGBA coverage grid moves to the worker and is shared through `Arc<Vec<u8>>`, within the previous maximum at nine bytes/cell as bounded above. Coverage completions no longer clone or retain the byte plane. Cell and presenter limits remain unchanged. |
| Point queries, validation and no-op checks | No new rasterization; frozen component evaluation and existing input-grid contracts remain in force. |
| Owner work | Bounded atomic preference reads/writes and the existing `job.cancel`; no frame work. |
| Desktop completions | Preferences, Cancel and idle samples add no asset/history fetch, preview job or pixel upload. Active edits retain the existing required paths. |
| Timers and polls | No new timer, subscription or worker thread. The existing after-message hook also drains ready coverage after publishing the photo; idle Performance-only updates use the scoped path. The paint slot blocks and wakes on release or cancellation. The existing one-second sampler is gated by expansion and visibility. |
| Unchanged work and caches | The existing coverage key and one-grid/input-grid bounds are unchanged. Cache hits share coverage rather than copying it; tests compare exact pixels and source release. Each GPU tile stores 96 bytes of last-written uniforms, bounded by the existing tile count; byte equality skips unchanged writes. No new image cache. |
| Photo-sized measurement | Matched native 24/60 MP `editor-latency --mode paint --mask-overlay --samples 240` measures this interaction and all worker legs. Results and limits are in [performance](../specs/performance.md#mask-feedback-and-the-coverage-handoff). A separate `editor-performance` core-render comparison is not claimed. |
| Exactness and sharing evidence | Frozen mask/reference and renderer tests, all 256 overlay codes against the old arithmetic, cache-hit painted bytes, source-release, handoff-bound/cancellation and progressive identity checks; native Metal surface readbacks and correlated mask/viewport captures. |

## Decisions

The owner's request authorizes performance fixes and the panel follow-ups above. Algorithm
quality, stroke representation and existing memory bounds remain the current contracts. A proposed
optimization that needs a consequential quality or extra-memory tradeoff remains a proposal.
