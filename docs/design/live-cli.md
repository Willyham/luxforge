# Live-session Rust CLI

Status: implemented ([feature status](../features.md), [user guide](../user-guide.md#the-live-session-command-line)).
This is the command-line client for people and agents that want to operate the photograph already
open in Luxforge without writing a custom JSON-lines or TCP client.

## Goal

A small Rust binary named `luxforge-ctl` in the existing `luxforge-cli` package.
It attaches to the desktop's authenticated loopback API and calls the same registry operations the
desktop uses. `luxforge-json` remains the separate headless catalog owner and is unchanged.

The CLI is a thin client. It does not render or edit pixels itself, launch another owner, bypass
validation, or write recipes/catalog data directly. A successful edit is a normal API transaction
with the same history and undo behavior as an edit made in the desktop.

## Targeting and safety

- **The running desktop, not the next launch.** Each desktop serving a live session registers it in
  a per-user registry, `<config>/live-sessions/` (resolved through `Paths`, without creating
  anything to read it): one owner-only entry `<pid>-<id>.json` naming the protocol, the process, the
  absolute catalog path and that catalog's session file, written whole, and beside it
  `<pid>-<id>.lock`, which the desktop holds locked (`std::fs::File::lock`) for as long as the entry
  is live. Without `--catalog`, the CLI uses the one running session there. An entry whose lock
  anyone else can take, or that has none, belongs to a process that has gone and is ignored and
  removed with its lock. No running session is `no-session`; several is a `usage` failure listing
  each catalog, since the CLI never guesses. `--catalog PATH` reads that catalog's session file
  directly. The desktop writes the entry beside the session file and removes it before the session
  file as it quits. The core owns the registry's format and its reader (`LiveSessionEntry`,
  `running_sessions`, `LocalServer::register`); the desktop's catalog selection
  (`CatalogSelection`, `CATALOG_FILE`) is the core's, beside `LaunchPreferences`, and the CLI no
  longer reads it.
- **The session file.** `catalog.with_extension("live-session.json")`, written to a temporary file
  created owner-only (0600 on Unix) and renamed over the path, so a reader never sees a partial file.
  The CLI accepts only the current `luxforge-jsonl-1` protocol and an IPv4 loopback address, and
  bounds its read, and a registry entry's, to 4 KiB. A registry entry must name the session file
  beside its catalog.
- **The handshake.** Connecting has a one-second timeout. The first request on every connection is
  `schema.list`, answered within five seconds and naming `luxforge-jsonl-1`, so an address that a
  stale file names and another program now holds can neither hang the CLI nor be mistaken for the
  session: a silent, closed, malformed or token-refusing answer is `stale-session`, another
  protocol `unsupported-protocol`. After the handshake there is no deadline on an answer: a
  legitimate long pixel read or held `job.wait` takes as long as it takes. A session already
  serving eight clients answers a ninth connection with one `resource-limit` failure
  (`data.max_clients`) before closing it, which the CLI keeps as answered.
- The CLI is an ordinary live client with Edit authority. It cannot grant permission, approve
  consent, or invoke setup-only operations. Never start `luxforge-json` or another catalog owner as
  a fallback.
- An asset is always explicit; the CLI never guesses from the desktop's selected photograph.
  Generate the API request `id` and each mutation `request_id` as UUID v4 values, and use the fixed
  actor label `luxforge-ctl`.
- **Envelopes from the schema.** Each method's `schema.list` entry names its envelope in
  `mutation` and, for a `revision` envelope, whose revision it carries in `revision_of`: `asset`
  (the asset its `asset_id` names), `draft` (the asset its draft is bound to) or `module-settings`.
  For a request-only mutation, insert `{request_id, actor}`. For `revision_of: asset`, require
  `--asset ID`, call `asset.state`, and insert that ID plus the just-read `expected_revision` and
  request metadata; for any other `revision_of`, require `--expected-revision N`. The envelopes are
  the core's `Mutation` and `MutationRequest`, serialized. Reject caller-supplied `mutation`, and
  `--asset` and `--expected-revision` where they do not apply. The asset-state read and the edit
  use one connection; if a human edit races between them, return the API conflict. Never retry,
  rebase or silently refresh a conflicting edit.
- **Session-scoped methods.** A method whose effect lives only in the calling client's session,
  which ends with its connection, is marked `session_scoped: true` in `schema.list`: the `draft.*`
  methods, `preview.select`, `preview.compare`, `preview.return-current`, `view.set`,
  `workspace.set`, `browse.select`, `browse.rows` and `job.adopt`. A method that also answers
  something useful alone, such as `browse.view` or `render.sample`, is not marked. `call` refuses a
  marked method with `usage`, pointing to `batch`.
- Preserve server errors, including their structured code, message and data. If a connection
  drops after a mutation was sent but before its response arrived, report the outcome as unknown;
  never automatically retry it.
- **Jobs and the connection.** `--wait` keeps one connection open and follows a returned job with
  `job.wait`'s change token and the session's longest hold, `MAX_EVENT_WAIT_MS` (30 seconds), until
  a terminal state. Every `job.read` record names its `ownership`: `catalog` for a job any client
  reads and no disconnect touches (an export, a module task, the catalog's listing, Develop and
  batch work), `clients` for one that belongs to the clients that requested it (source work, an
  analysis, a shared preview read or render), which stops when the last interested client cancels
  or disconnects and which no other client can read. So exiting, or interrupting a `--wait`, stops
  a `clients` job its CLI alone wanted. When a request without `--wait` returns a job, the CLI
  reads its `job.read` and, for a `clients` job, warns with `job-released` that it will be released
  when the CLI exits (or the batch ends).

## v0 command contract

```text
luxforge-ctl [--catalog PATH] status
luxforge-ctl [--catalog PATH] schema [METHOD]
luxforge-ctl [--catalog PATH] call METHOD [--params JSON | --params @FILE | --params -] [--asset ID | --expected-revision N] [--wait]
luxforge-ctl [--catalog PATH] batch < REQUESTS.jsonl
```

A small hand-written argument parser consistent with `luxforge-json`, with no CLI parsing
dependency for this four-command interface.

- `status` returns `{session, catalog}` from `session.state` and `catalog.info`.
- `schema` returns the handshake's `schema.list` result. `schema METHOD` returns that method's
  entry from the result's methods map, or a validation error when absent. The CLI does not maintain
  a second list of operation parameters.
- `call` sends a method with a JSON object, defaulting to `{}`. `--params JSON` accepts inline JSON,
  `--params @FILE` reads one UTF-8 JSON document from a file (relative to the caller's working
  directory), and `--params -` reads it from stdin; it is bounded to the 1 MiB request limit and
  checked before connecting. The handshake's `schema.list` validates the method and gives its
  envelope. Reject a non-object params value, duplicate flags, unknown flags, a missing required
  argument, an unknown method and a session-scoped method before sending the target operation.
  `--help` and `--version` do not connect to Luxforge.
- `batch` reads JSON lines from stdin, each `{method, params?, asset?, expected_revision?, wait?,
  id?}` with the meanings of `call`'s arguments, and sends them in order over one connection after
  one handshake, so session-scoped methods work across requests. Each request is answered with one
  stdout line, `{id?, result, warning?}` or `{id?, error, job? | call?}`, `id` echoed. A failed
  request, or an unreadable line (`usage`), is answered and the batch continues; a lost connection
  ends it, since nothing more can be sent. Lines are bounded to 1 MiB, the rest of a longer line
  read and dropped; blank lines are skipped.
- Bound each response line at 64 MiB and fail explicitly above it to keep client memory bounded.
  Validate a fully assembled target request against the 1 MiB protocol line limit
  (`MAX_REQUEST_BYTES`, the core's) before writing it to the connected socket.
- Print successful responses as JSON on stdout. Print structured errors, and warnings beside a
  result, on stderr. Diagnostics never contaminate stdout.

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

## Decisions

The binary is named `luxforge-ctl`; without `--catalog` it attaches to the one running desktop's
session from the registry of running sessions, and includes the schema-driven `--asset` revision
helper. No operation-specific aliases are included.

Implementation choices this contract left open, recorded as built:

- **Errors.** The error type is the API's own `ApiFailure`, so an owner's failure is printed
  exactly as answered. The client's own codes are `usage`, `validation`, `no-session`,
  `stale-session`, `invalid-session`, `unsupported-protocol`, `resource-limit`, `protocol` (an
  answer that is malformed or for another request, or a schema entry naming an envelope or
  `revision_of` this client cannot build), `internal` (a request it could not encode),
  `connection-lost` (a read) and `outcome-unknown` (a mutation, with its `method` and `request_id`
  in `data`).
- **Exit status.** 0 success; 1 any failure not listed here, the owner's own included; 2 `usage`;
  3 no session to speak to (`no-session`, `stale-session`, `invalid-session`,
  `unsupported-protocol`); 4 `outcome-unknown`. A batch exits with the highest status of its
  requests. A full session's `resource-limit` is the owner's answer and exits 1.
- **Warnings.** A warning has the failure's shape, `{code, message, job_id, data}`: on stderr as
  `{"warning": …}` beside a `call`'s result, and as `warning` in a batch line. Its only code is
  `job-released`, with the job's `kind`, `status` and `ownership` in `data`.
- **Output.** A result is printed as one compact JSON line. `--wait` prints `{call, job}`; a job that
  ends other than `ready` is an error carrying the job's own error, with the record beside it as
  `job`. A result that names no job prints alone.
- **Parameters.** With `--asset`, `asset_id` in `--params` is refused, so the asset is named once.
