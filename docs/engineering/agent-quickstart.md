# Agent quickstart

What an agent runs, and when, while implementing a change. The [development guide](development.md)
is the reference behind every command here. Read it only for the command or check you need.

The host is shared. Several sessions build and test on the owner's Mac at once, so every extra
build, whole-suite run or rerun slows everyone else's down.

## Builds

- Run every Cargo command through `tools/cargo-cached`. At most three building commands run at
  once across the host. When it reports that every build slot is busy, it is queueing your
  command: wait, and never bypass it with plain `cargo`
  ([build slots](development.md#the-build-volume-and-build-slots)).
- Run one building command at a time. Do not start Clippy, a test run and an `xtask` build side by
  side, or leave one running in the background while you start another.
- New worktree: run `tools/cargo-cached xtask doctor` once.

## While editing

- Test only the code you are changing:
  `tools/cargo-cached test -p CRATE --lib FILTER`, or `--test NAME FILTER` for one integration
  binary. Never `--workspace`, `check` or `verify` in the edit loop.
- Lint one crate when you need lint feedback:
  `tools/cargo-cached clippy --locked -p CRATE --all-targets -- -D warnings`.
- Rendered change: run the one smoke scenario that covers it against a current release build:
  `tools/cargo-cached build --release --locked -p luxforge-app`, then
  `tools/cargo-cached xtask smoke --scenario NAME --output artifacts/NEW_DIR`.
- Change to what the GPU draws: `gpu-qualification` with a selection (`--families`, `--recipes`,
  `--sources`, `--zoom`) of the stacks you touched. A selection never counts as the gate's pass.
- Timing runs wait until the feature is complete, unless the task is performance work
  ([when to verify](development.md#when-to-verify)).

## Before hand-off, once

1. `tools/cargo-cached fmt --all`
2. `tools/cargo-cached xtask check-repository`
3. `tools/cargo-cached clippy --locked -p CRATE --all-targets -- -D warnings` for each crate you
   changed. Steps 2 and 3 take seconds to a minute and catch most failures `quick` would
   otherwise find several minutes in.
4. `tools/cargo-cached xtask verify --tier quick --output artifacts/NAME-quick`

For a UI or image change, also look at the scenario's captures as images. Launch the editor only
through the background harness or `xtask develop --background`, and drive and inspect it through
the live API and renderer readbacks. Foreground interaction checks need the owner's explicit
request. For a change under `crates/`, answer the [performance rules](performance-rules.md)
checklist.

If `quick` fails because of your change, fix it, rerun only what failed (the named test or the
check), then run `quick` once more. If a failure cannot have come from your change, such as a
watcher or disk-image timeout in code you did not touch, do not investigate it and do not rerun
to get a pass. Report it as failed, by name, and carry on.

A documentation-only change runs none of the tiers: run `xtask check-repository`, `xtask lint`
and `fmt --all -- --check`.

## Docs

- Update the spec or design of the area you changed.
- The shared documents are [feature status](../features.md), the [user guide](../user-guide.md),
  the [roadmap](../plan.md), this guide and the [development guide](development.md), and the
  [task plans' README](../../tasks/README.md). When an integrator merges your work with other
  agents' work, do not edit them, even when a task's acceptance names them. Say in your hand-off
  what each one needs, and the integrator updates them once per feature. Working alone, you are
  the integrator: update them once, at the end.
- Document current behavior only. Measurements and run results belong in your evidence directory
  and your report, not in the docs. A doc states a figure only when it is a budget, a limit or the
  basis of a recorded decision.

## Tests

- Write one test per behavior, at the cheapest level that proves it.
- A test that takes 100 ms or more, such as an exhaustive sweep, is named
  `slow_`: `quick` skips it and the whole `check` runs it. A numerical study belongs in
  `luxforge-reference`.
- A test whose timing belongs to a host service (FSEvents, inotify, disk images, an editor
  launch) needs a seam it can drive without that service in the unit tier. Name the test that
  uses the real service `slow_`, or make it a smoke scenario.
- A test sets up any process-wide state it reads, such as an installed output encoding, itself.
  `quick` and a filtered run leave tests out, so the test that would have installed it may never
  run first.
- Order comes from gates and waits, never from sleeping
  ([host-load-independent tests](development.md#tests-that-do-not-depend-on-host-load)).

## Hand-off report

Give the exact commands and their results: passed, failed, skipped or not run. A skip is never a
pass. Include the artifact paths, any unrelated failures by name, the shared-doc updates the
integrator owes, and anything the acceptance asked for that you did not do. Mark a task done
only when its acceptance is met.

## Integrators

- Run `rendered` once on the integrated branch when the work touches rendering or the UI.
  Run `timing` when a change under `crates/` can affect cost. Run `full` with `--manifest` before
  a milestone claim. Only `full` runs `visibility-monitoring` and the `gpu-qualification` release
  gate.
- Update the shared docs once per feature, from the implementers' hand-offs.
- At most three agents build at once across the host; the build slots queue the rest. Work that
  builds nothing, such as planning, review or docs, is not limited.
- After merging into `main`, clean up the finished worktrees, as the agent instructions'
  [clean-up rule](../../AGENTS.md) says.
