# Isolated rendering performance

Scope: the first implementation wave targets three measured hot spots without changing photo output, editing semantics, recipes, API shapes, scheduling or memory targets. A separate native-demosaic assessment determines the smallest bounded implementation route. GPU colour and larger spatial tiles remain proposals; the latter's measured readout penalty is not implicitly accepted by this work.

## Measured motivation

On the native M4 Pro, release at `22c4e90`, a generated 60 MP JPEG with all Presence fields at +100 takes about 3.64 s. `Plane::get` accounts for about 39% of active samples; its assembly retains per-read coordinate clamps, address arithmetic and an out-of-line call. Prepared linear rendering at 60 MP takes about 198 ms, with roughly 72% of active samples in terminal sRGB conversion and its power function. Basic's bounded hue weighting spends about 4% in a redundant floating remainder. These are diagnostic costs on a live host, not promised gains.

Actual Fuji development from a retained mosaic takes about 1.28 s on one core, with about 87% of samples in Markesteijn. The native build disables existing OpenMP tile loops. Turning them on introduces a second scheduler and is not automatically compatible with the shared Rayon pool rule.

## First implementation wave

1. Expose Presence scalar-read invariants to the compiler, starting with the smallest inlining change and using safe row spans only if needed. Preserve edge clamping, pixel arithmetic, f64 filter accumulation, tile sizes, parallelism and scratch bounds. Retain a change only when a targeted comparison shows a benefit.
2. Remove repeated RAW terminal power functions by reusing the existing sRGB code-boundary approach, while preserving the previous terminal result at rounding boundaries, non-finite rejection and the extended linear domain before terminal conversion. Do not change JPEG quantization or loosen exact tests to make a candidate pass. If necessary, retain a narrow canonical fallback near ambiguous boundaries, or report the candidate as unsuitable.
3. Specialize Basic's hue weighting for the bounded result of atan2 minus its fixed centre so the redundant remainder disappears. Keep general normalization semantics where they are needed and preserve current bytes, including signed/near-zero cases.
4. Assess bounded native demosaic parallelism through the existing adapter and shared worker pool. Record a concrete design and constraints; introducing a private OpenMP runtime or porting the whole demosaicer is not part of this first wave.

Independent edits run in separate Git worktrees. Each implementation agent owns its narrow source area and focused tests. The integrator owns cross-cutting docs, the task plan, performance evidence and final acceptance. Build/test work may overlap; benchmark work is scheduled exclusively so agents do not measure one another's CPU load.

## Acceptance

- Exact current image results and sample/render parity are preserved. Compare with independent references or the previous implementation, including finite rounding thresholds, borders, masks, spatial tiles and RAW headroom as applicable.
- No original reads, allocations scaling with image area, queues, timers, owner-thread work or runtime hops are added by these kernel changes. Existing resource targets and cancellation remain unchanged.
- Each agent runs targeted tests while editing and quick verification once at handoff. The integration run covers rendered and timing evidence; full with the local RAW manifest supplies the relevant native journeys without treating a skipped check as a pass.
- Final timings use release code, photo-sized 24/60 MP inputs, warm-source scope, explicit sample counts and host load. The before and after binaries are retained and run serially, in both orders when host variability affects the conclusion. Do not add CI timing gates.
- Document implemented behavior and remaining costs in the performance specification, feature status and user guide where relevant. No speedup is claimed from a profile fraction alone.

## Open decisions

Larger spatial tiles, changed point-sample scheduling, approximation tolerances, a GPU colour backend and an additional native parallel runtime remain separate decisions. The initial scope seeks exact, local CPU gains before those broader changes.
