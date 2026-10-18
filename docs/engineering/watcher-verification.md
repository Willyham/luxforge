# Watcher verification

Status: implemented and verified with quick and the separate native gate on macOS. Native
Windows/Linux and hosted CI execution are unrun. Watcher coverage in quick is cheap and deterministic, with a
separate native integration gate required for watcher changes and releases.

## Contract

- Ordinary tests cover event interpretation, bounded delivery, signature reconciliation, rename
  identity, exclusions, rescan jobs, cancellation, stale-root recovery, cursor persistence and
  interrupted reads. Quick keeps the cheap deterministic checks; injected journeys taking 100 ms
  or more carry `#[slow]`, including those exercising the deletion grace period. Core journeys use real scratch files and the production owner/index lane, with an
  explicitly injected notification source and a fixture-owned cursor. No native notification
  delivery is needed to complete them.
- Initial root delivery that a full channel refuses explicitly wakes an idle Linux or Windows
  wait, then retries the owed rescan and caught-up in order. A deterministic delivery check
  covers the wake decision; platform native checks exercise the eventfd and completion-port wake.
  Linux queue-overflow tests restore watches for missed folder arrivals and renames before the
  rescan is reported, preserve nested-root sharing within the kernel watch budget, and retain
  both the rescan and an unwatched report when rebuilding fails.
- Tests that require real platform delivery have the own-name prefix `native_watch_` and carry `#[slow]`. Ordinary
  `xtask check` and `xtask test`, quick and full, report and exclude those tests; full ordinary
  checks still include slow tests and doctests. Direct Cargo tests retain their usual behavior.
- The dedicated `xtask test --native-watchers` gate selects only `native_watch_` tests, fails if none
  are available, and preserves failures rather than retrying them into a pass. Its test binaries
  run separately from ordinary tests and one at a time. The usual hang bounds apply.
- The native suite holds the platform adapter's real delivery, replay and removal checks and
  core journeys proving real automatic updates, close/reopen replay and interrupted-read catch-up.
  The fixture's automatic change/replay injection is disabled in these journeys. The interrupted
  read uses explicit fault injection before close and requires real OS catch-up after reopen.
  Disk-image and other ignored qualification checks keep their own explicit scope; they
  are not silently claimed by this gate.
- Non-quick verification tiers record the native gate as a separate required component. Hosted
  checks and tag builds run it explicitly after ordinary checks. Local release initiation requires
  it before creating or pushing a tag. Watcher changes require quick plus the native gate on the
  changed platform; release qualification requires the gate on each release platform.

The injected source is private and `cfg(test)` only, owned by one fixture and restored before
opening that fixture's catalog. It records roots and their requested resume cursors, supplies a
fixed synthetic current cursor, and sends startup rescan/caught-up events through the lane's
bounded channel. Tests explicitly report subsequent changes. It does not invent platform replay
history or change production watcher, catalog, recipe or image behavior.

## Acceptance

Selection tests prove that native and ordinary tests are disjoint, that prefix matching uses the
test's own name, and that an empty native selection fails. Core tests prove the same catalog
invariants with injected input, including the cursor offered when reopening. Native journeys
remain real OS tests and are reported independently, with an isolated pass never qualifying a
failed aggregate native run. Quick passes without native delivery. No renderer or performance
claim is made; Windows/Linux native qualification is only claimed when actually run.

Routine test-source and CLI organization is delegated within this contract. Delivery-stall
diagnosis remains outstanding if the native gate fails; it does not block building the partition.
