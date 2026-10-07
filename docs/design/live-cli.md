# Live-session Rust CLI

Status: specified; implementation awaits owner authorization. This is the command-line client for
people and agents that want to operate the photograph already open in Luxforge without writing a
custom JSON-lines or TCP client.

## Goal

Add a small Rust binary named `luxforge-ctl` to the existing `luxforge-cli` package.
It attaches to the desktop's authenticated loopback API and calls the same registry operations the
desktop uses. `luxforge-json` remains the separate headless catalog owner and is unchanged.

The CLI is a thin client. It does not render or edit pixels itself, launch another owner, bypass
validation, or write recipes/catalog data directly. A successful edit is a normal API transaction
with the same history and undo behavior as an edit made in the desktop.

## Targeting and safety

- `--catalog PATH` explicitly selects a catalog. Otherwise, read the stored catalog from
  `LaunchPreferences`; use it when its parent directory exists, and use `<config>/catalog.sqlite`
  when no catalog is stored or the stored parent is missing. This matches the desktop's ordinary
  launch fallback. Factor the path-selection rule into UI-independent shared code and have both the
  desktop and CLI use it, so their defaults cannot drift. Resolve platform config through the
  existing `Paths` implementation without creating directories or files.
- Read the catalog's live-session sidecar, validate its protocol marker and connect only to its
  loopback address with its per-run token. The sidecar path is
  `catalog.with_extension("live-session.json")`; accept only the current `luxforge-jsonl-1`
  protocol and an IPv4 loopback address. Bound the sidecar read to 4 KiB and each outgoing request
  line to the transport's 1 MiB limit. If the selected catalog is not open, the sidecar is stale,
  or the protocol is unsupported, stop with a clear error. Never start `luxforge-json` or another
  catalog owner as a fallback.
- Use a one-second connection timeout. Once connected, wait for the API response until it arrives
  or the user interrupts the process; do not impose a deadline on a legitimate long pixel read.
- The CLI is an ordinary live client with Edit authority. It cannot grant permission, approve
  consent, or invoke setup-only operations.
- An asset is always explicit; the CLI never guesses from the desktop's selected photograph.
  Generate the API request `id` and each mutation `request_id` as UUID v4 values, and use the fixed
  actor label `luxforge-ctl`.
- Read `schema.list` to determine each method's mutation envelope. For request-only mutations,
  insert `{request_id, actor}`. For revisioned asset mutations (the schema's required fields include
  `asset_id`), require `--asset ID`, call `asset.state`, and insert that ID plus the just-read
  `expected_revision` and request metadata. For revisioned mutations without an asset ID, require
  `--expected-revision N` and use it in the envelope. Reject caller-supplied `mutation` when the
  CLI builds it. The asset-state read and edit use one connection; if a human edit races between
  them, return the API conflict. Never retry, rebase or silently refresh a conflicting edit.
  Read-only methods pass parameters through unchanged. Reject `--asset` and
  `--expected-revision` when their respective schema rules do not apply.
- Preserve server errors, including their structured code, message and data. If a connection
  drops after a mutation was sent but before its response arrived, report the outcome as unknown;
  never automatically retry it.
- Keep one connection open while `--wait` uses `job.wait`'s change token and a maximum 30-second
  hold to follow a returned job. Continue until a terminal job state; interruption does not
  silently cancel the job.

## v0 command contract

Build a second binary in the existing package, without GUI dependencies:

```text
luxforge-ctl [--catalog PATH] status
luxforge-ctl [--catalog PATH] schema [METHOD]
luxforge-ctl [--catalog PATH] call METHOD [--params JSON | --params @FILE | --params -] [--asset ID | --expected-revision N] [--wait]
```

Use a small hand-written argument parser consistent with `luxforge-json`; do not add a CLI parsing
dependency for this three-command interface. Add the existing pinned workspace `uuid` dependency
directly to `luxforge-cli` for UUID v4 request IDs.

- `status` returns `{session, catalog}` from `session.state` and `catalog.info`.
- `schema` returns the full `schema.list` result. `schema METHOD` returns that method's entry from
  the result's methods map, or a validation error when absent. The CLI does not maintain a second
  list of operation parameters.
- `call` sends a method with a JSON object, defaulting to `{}`. `--params JSON` accepts inline JSON,
  `--params @FILE` reads one UTF-8 JSON document from a file, and `--params -` reads it from stdin.
  A relative `@FILE` path is resolved from the caller's working directory.
  For mutations, generate the required envelope from `schema.list` as described above. `--asset`
  is mandatory for revisioned asset mutations and forbidden for other methods. For revisioned
  mutations without a required `asset_id`, `--expected-revision N` supplies the revision; it is
  forbidden for other methods. The two options are mutually exclusive.
- Fetch `schema.list` once per `call` to validate the method and determine whether it is read-only,
  has a request-only mutation envelope, or has a revision envelope. Add request IDs and actor to
  every generated mutation envelope. For a revision envelope whose required fields include
  `asset_id`, require `--asset`, fetch `asset.state` over the same connection, then construct
  `asset_id` and the mutation envelope. For any other revision envelope require
  `--expected-revision`. Reject caller-supplied `mutation`, a non-object params value, duplicate
  `--params`, unknown flags, a missing required argument and an unknown method before sending the
  target operation. `--help` and `--version` do not connect to Luxforge.
- Bound each response line at 64 MiB and fail explicitly above it to keep client memory bounded.
  Validate a fully assembled target request against the 1 MiB protocol line limit before writing
  it to the connected socket.
- Print successful responses as JSON on stdout. Print structured errors on stderr and exit
  non-zero. Diagnostics never contaminate stdout.

The executable is named `luxforge-ctl`, the revision helper is included, and no operation-specific
aliases are included. This narrow contract provides one discoverable client first; aliases can be
considered only after real command use shows a need.

## Acceptance

- A command targets the same running catalog owner as the desktop and can read live state and
  schema, make a revision-checked edit, and wait for a long-running job.
- No-session, stale-sidecar, malformed protocol, authentication, validation and revision-conflict
  failures are clear and preserve API error structure.
- Requests stay within the existing bounded JSON-lines protocol and loopback transport; no
  permission authority or direct catalog writes are added.
- The command output is scriptable, and the user guide documents only commands that exist after
  implementation.

## Decisions resolved for this plan

The owner asked that the plan be implementable without further questions. This plan therefore fixes
the binary name as `luxforge-ctl`, uses the desktop's stored/default catalog selection with an
explicit `--catalog` override, and includes the schema-driven `--asset` revision helper. The owner
has asked for this plan to need no further product answers. Implementation itself still awaits a
separate go-ahead.

The task plan is [live-cli.json](../../tasks/project/live-cli.json). TASK-001 is ready in the
roadmap's authorization section; implementation awaits the owner's go-ahead.
