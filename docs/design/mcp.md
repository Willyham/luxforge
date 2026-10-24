# MCP adapter

Status: planned, awaiting implementation authorization ([task plan](../../tasks/programmability/mcp.json)). Nothing here is built yet.

`luxforge-mcp` is a Model Context Protocol server. It lets an agent (Claude Code, Claude Desktop, Codex, Cursor and other MCP clients) operate Luxforge through the same operation registry that the desktop, `luxforge-json` and `luxforge-ctl` use. It is an adapter and has no feature logic of its own. Every tool it offers is a registry method, every edit is an ordinary API transaction with the same history, undo and conflict behaviour as a person's, and every argument schema is generated from `schema.list`. There is no hand-written copy.

## Goal

A person adds Luxforge to their agent once, from **Settings › Agents** or the user guide. From then on, while Luxforge is open, the agent can:

- read the catalog and the open photograph;
- make edits that land in history under the agent's name;
- wait for jobs;
- see the result as a picture.

All of this uses the operations the person has. An agent cannot do anything the live session cannot do. In particular, it can never grant consent or permission.

## Shape

- **One binary.** `luxforge-mcp` is a third binary in the `luxforge-cli` package. It reuses `luxforge_cli::live` (session discovery, connection, handshake and envelope construction) and keeps the package's headless rule: no Iced, wgpu or desktop crates.
- **Transport.** The only transport is stdio, JSON-RPC 2.0, one message per line. The MCP client starts the process and owns its lifetime.
  - Diagnostics go to stderr only.
  - There is no Streamable HTTP listener, because a local network endpoint would need its own origin checks and authentication, and stdio is what local clients launch.
  - The official conformance suite tests servers over HTTP and runs on Node. The project keeps all tooling in Rust, so [verification](#verification) uses Rust tests against the vendored specification schemas and an independent Rust client instead.
- **Implementation.** The protocol layer is hand-written: synchronous, over `serde_json`, with a reader thread and one worker, in the style of `luxforge-ctl`. The surface it needs is small: discovery, the legacy handshake, `tools/list`, `tools/call`, cancellation and list-change subscriptions. `rmcp`, the official Rust SDK, was considered as the server. It releases roughly weekly at 3.x and brings an async server stack this adapter does not need, so it is used only as an independent test client ([verification](#verification)).

## Protocol versions

The adapter is **dual-era**, as the [2026-07-28 versioning rules](https://modelcontextprotocol.io/specification/2026-07-28/basic/lifecycle) define it. That revision is the current one: it is stateless and has no `initialize` handshake. Many clients still speak 2025-11-25.

- **Modern (2026-07-28).**
  - Each request carries `io.modelcontextprotocol/protocolVersion`, `clientCapabilities` and, optionally, `clientInfo` in `_meta`. Each result carries `io.modelcontextprotocol/serverInfo` and `resultType: "complete"`.
  - `server/discover` advertises the supported versions, the `tools` capability (`listChanged` in the typed profile) and the server's identity.
  - An unsupported version gets `UnsupportedProtocolVersionError` (`-32022`), listing the supported versions.
  - `tools/list` returns `ttlMs` and `cacheScope: "private"` and lists tools in a deterministic order.
  - `subscriptions/listen` serves `toolsListChanged`.
  - `notifications/message` is never sent, because Logging is deprecated.
- **Legacy (2025-11-25 and 2025-06-18).**
  - An `initialize` request selects legacy semantics for the life of the process. The server negotiates the client's version when it supports it, and otherwise answers with 2025-11-25.
  - It then serves `tools/list`, `tools/call`, `ping` and `notifications/tools/list_changed` as those revisions define them.
  - Results never carry `resultType`, and `_meta` is not required.
- **Both eras.**
  - `notifications/cancelled` for a queued request drops it. For a running call, it stops the adapter's own waiting at its next hold. It never cancels a Luxforge job: that is `job.cancel`, an operation of its own.
  - There are no resources, prompts, sampling, roots, elicitation or tasks. Luxforge jobs stay jobs, read with `job.read` and `job.wait`. The `io.modelcontextprotocol/tasks` extension is later work.
- **One call at a time.** Requests are served one at a time, in arrival order, on one live connection, which keeps session-scoped state coherent. At most 32 requests are queued; beyond that a request is refused with a JSON-RPC error that names the limit.
  - An incoming line is bounded at 1 MiB, as the owner's request limit is.
  - A held wait, such as `job.wait` or `events.wait`, is at most 30 s, so a queued call waits at most that long behind it.

## Attaching to Luxforge

- **The running desktop.** By default the adapter attaches to the running desktop's live session, through the same registry of running sessions and `--catalog PATH` rule as `luxforge-ctl` ([live CLI](live-cli.md#targeting-and-safety)). It is an ordinary live client with Edit authority.
- **Attaching late.** Agents often start their MCP servers before Luxforge is open, so the adapter starts, answers discovery and lists its tools without a session.
  - It attaches at the first tool call that needs the owner. With no session, that call returns a tool error, `no-session`, saying to open Luxforge. With several sessions it returns `usage`, listing each catalog, since the adapter never guesses.
  - When the connection is lost (the desktop quit or restarted), the in-flight call reports `connection-lost`. A mutation reports `outcome-unknown`, with its method and `request_id`, and is never retried.
  - The next call discovers the session again. The first result after a reconnect carries a `session-replaced` warning: drafts, previews and `clients` jobs belonged to the old connection and are gone.
- **Identity.** On connecting, the adapter names itself with `session.identify` ([below](#core-additions)), using the MCP client's `clientInfo` title or name followed by "via MCP", for example "Claude Code via MCP". Its mutations use the actor `mcp:<client name>`, bounded to the envelope's 128 characters, or `mcp` when the client gives no name, so a history row shows which agent made the edit.
- **Headless.** `--headless --catalog PATH` starts the adapter's own `luxforge-json --catalog PATH` child, with Edit authority and never `--permission-authority`, and speaks to it over the child's stdio.
  - It refuses with a named error when a running desktop's registry entry names that catalog. The catalog's exclusive SQLite lock refuses a second owner anyway; the registry check makes the message clear.
  - The child lives exactly as long as the adapter. On stdin end-of-file the adapter closes the child's stdin, waits a bounded time, then kills it. The child's stderr is forwarded to the adapter's stderr.
  - A headless owner draws nothing, so it renders on the reference renderer and `session.state` reports that. A method a headless owner cannot serve fails explicitly as it already does.

## Tools

One generator turns the live `schema.list` into tools. Which tools are offered is chosen with a launch flag, `--tools`:

| `--tools` | Tools offered |
| --- | --- |
| `compact` (default) | Three generic tools, a fixed list whether or not a session exists |
| `all` | One typed tool per registry method |
| `NS[,NS…]` | One typed tool per method in the named namespaces, e.g. `asset,edit,query,render,history,job,preview` |

The default is compact, because the full registry is 233 methods. Their notes and parameters are about 275,000 characters, roughly 75,000 tokens of tool definitions. Typed tools are opt-in, for clients that defer large tool lists or for a person who wants to approve operations one by one.

### The compact profile

- **`luxforge_methods {namespace?, query?}`** lists methods, in registry order. Each entry has its name, a one-line summary (the first sentence of its notes), `mutates`, `session_scoped`, its effect markers and whether a live client can call it at all. The query filter matches names and notes, case-insensitively. Read-only.
- **`luxforge_describe {methods: [name…]}`** returns, for each named method (up to 16), its generated input schema, its full notes and its envelope. Read-only.
- **`luxforge_call {method, params?, expected_revision?}`** calls any method. Before sending, it validates `params` against that method's generated schema and refuses a method a live client cannot call. It builds the envelope as described under [envelopes](#envelopes) and returns the result as described under [results](#results).
  - Its annotations are the conservative defaults (`readOnlyHint: false`, `destructiveHint: true`, `openWorldHint: true`), because it can call anything.

Without a session, `luxforge_methods` and `luxforge_describe` answer from the built-in registry of the adapter's own build and say so in a `source: "built-in"` field. The live registry can differ, for example under developer mode or an experiment.

### The typed profiles

- **One tool per method.** A tool's name is the method's name with `.` replaced by `_`, because some model APIs refuse dots in tool names; `asset.state` becomes `asset_state`. A test holds the mapping collision-free and every name within 64 characters. The tool's `title` is the method's own name.
- **Description.** The description is the method's full notes.
- **Annotations.** These come from the registry's markers ([core additions](#core-additions)):
  - `readOnlyHint` is the method's `read_only`.
  - `destructiveHint` is true only for a method that changes something and cannot be taken back: `!read_only && !reversible`.
  - `idempotentHint` is false.
  - `openWorldHint` is true only for a method marked as reaching the network.
- **Methods left out.** A method that only permission authority can call is not offered, because a live client's call would always be `forbidden`.
- **Without a session.** The list comes from the built-in registry. Once attached, if the live registry differs, the adapter announces a list change.
- **List changes.** The registry changes when a flag or developer mode adds or removes methods. Once the agent has subscribed (modern) or after `initialize` (legacy), the adapter watches the owner's event log on a second live connection, with a held `events.wait`. After an event from `flags.set`, or from module enablement, it reads `schema.list` again and sends `notifications/tools/list_changed` if the generated list differs.
  - A headless child serves one connection, and in headless mode the adapter is the only client. So it reads the schema again only after its own calls to those methods.

### Schema generation

The generator turns each method's `parameters` (the [parameter vocabulary](architecture.md#agent-contract)) into a JSON Schema 2020-12 object schema:

- **Properties.** Each parameter becomes a property, with `required` taken from the descriptor and `additionalProperties: false`.
- **Descriptions.** A property's description holds its notes, unit, step, soft range and precision.
- **Mapping by kind:**
  - `integer` and `number` become `integer` and `number`, with `minimum` and `maximum`.
  - `enum` becomes `enum`.
  - `boolean` becomes `boolean`.
  - `string` becomes `string` with `maxLength`.
  - `text` becomes `string`, with its byte bound stated in the description and checked by the adapter, because `maxLength` counts characters, not bytes.
  - `identity` becomes the identifier's wire type, described as "a *kind* id".
  - `points` and `curve` become arrays of pairs with `minItems` and `maxItems`.
  - `color`, `artifact`, `settings` and `settings-origin` follow their wire forms.
  - `json` becomes `{}` (any value), with its notes as the description. The 75 `json` parameters give their shape only in prose today; publishing those shapes is [later work](#later).
  - `default` is copied.
- **Host fields.** A field an entry lists beside its declared parameters is typed as the host takes it: `asset_id` an asset id, `entry_id` an entry id, `profile_id` a string of at most 256 characters, and a maskable module's mask by the entry's own `target`. A field nothing types refuses the entry.
- **`null`.** An optional field of a host method also accepts `null`, which the host reads as absent or, for a field that can be cleared, as clearing it (`preferences.set {theme: null}`). An action's, query's or task's fields do not, because their check refuses it.
- **Envelopes.** The envelope never appears as a property ([envelopes](#envelopes)).
- **Unknown kinds.** A parameter kind the generator does not know fails a test, so a new kind cannot ship without its mapping. Live, it refuses the whole `schema.list` answer by name rather than offering a method with a guessed schema.
- **The request check.** `luxforge_cli::mcp::schema` also checks a call's arguments before anything is sent, with the core's own value check, so it refuses in the owner's words and names the parameter. It covers what a schema cannot say: a `text` bound in bytes, a `string`'s control characters, an identity's syntax, a curve's order and a settings set's action names. A generated schema never refuses what the check accepts.

### Envelopes

The adapter builds every mutation envelope from the method's `schema.list` entry, exactly as `luxforge-ctl` does:

- **Request IDs and actor.** The adapter generates `request_id` (UUID v4) and supplies `actor`. An agent cannot pass `mutation`.
- **`revision_of: asset`.** `expected_revision` is an optional extra property. When the agent passes it, it is used as given, so an agent that read state, reasoned and then edited gets an honest `conflict` if the person edited in between. When omitted, the adapter reads `asset.state` on the same connection immediately before the edit, as `luxforge-ctl --asset` does.
- **`revision_of: draft` and `module-settings`.** `expected_revision` is a required property.
- **Conflicts.** A conflict is returned as a tool error. The adapter never retries, rebases or refreshes an edit.

Session-scoped methods (`draft.*`, `preview.select`, `view.set` and the rest) are offered in every profile. The adapter keeps one connection for its whole life, so a draft begun in one tool call is there in the next. Drafts are explicit handles (`draft_id`), which is how the stateless revision wants cross-call state carried. Their lifetime, the adapter's process and its connection, is stated in those tools' descriptions.

### Results

- **Success.** A successful call returns the method's result as `structuredContent` and the same JSON as one text content block. There is no `outputSchema`, because the registry publishes result shapes only in notes.
- **Failure.** A Luxforge failure is a tool execution error (`isError: true`). Its text and `structuredContent` are `{error: {code, message, data?, job_id?}}`, preserved exactly, so a model can correct itself from `validation`, `conflict`, `preparation-required` or `consent-required`.
- **Protocol errors.** An unknown tool is JSON-RPC `-32602`. Arguments that fail the generated schema are a tool execution error naming the parameter.
- **Jobs.** A result that names a job is returned as the registry returns it. The agent follows it with `job.wait`, whose hold is at most 30 s. A job whose `ownership` is `clients` lives as long as the adapter's connection, and the result says so with a `job-released` style warning as `luxforge-ctl` does.
- **Pictures.** A result field that the registry marks as an image ([core additions](#core-additions)) is read by the adapter and attached as MCP image content (`image/jpeg`, base64), beside the structured result.
  - This covers `preview.read`'s path and fallback, and the path of a preview job's result (`preview.region`'s among them, which answers only a job) in a `job.read` or `job.wait` result.
  - The largest photo tier is 2048 px, so a picture is at most a few hundred kilobytes. The adapter refuses to attach a file over 8 MiB, and the result carries a warning instead.
  - Full-resolution exports are never attached. Their paths are returned as data.
  - `--no-images` turns attachment off for a launch.
  - The user guide says plainly that an agent's model provider may be remote, so an attached picture leaves the machine with the agent's other context.
- **Size.** Results are bounded by the live client's 64 MiB response limit and are never truncated. A list too large for an agent's context is the agent's to page, using the method's own paging parameters.

## Core additions

The registry gains the markers the adapter needs, so the adapter holds no per-method knowledge.

- **`authority: "permissions"`** on each method that only permission authority may call: `module.permission.grant`, `usage.prompt.claim`, `update.install`, `update.move-to-applications`, `catalog.empty-removed` and `session.disconnect`. A method that needs permission authority for one argument value, such as `usage.set {enabled: true}`, keeps that in its notes and stays offered.
- **Effect markers** on every method. `mutates` says only that a method carries a mutation envelope. Many methods change something without one (`preferences.set`, `view.set`, `draft.*`, `job.cancel` and the rest), so whether a method changes anything is `read_only`:
  - `read_only: true | false` on every method, true only when it changes nothing at all: no catalog, library, session, preference, flag, job or file state. Holding a wait is a read, and so is a call whose only effect is a derived, disposable record such as a preview, an analysis report or the folder index ([the rule](architecture.md#agent-contract)).
  - `reversible: true | false` on every method whose `read_only` is false, and on no read. A reversible change can be undone through history undo or library undo, set back by the same method with the earlier value, removed with what the call created, or is the caller's own session state, set again.
  - `network: true` for a method that can reach the network, never a read.
  - Every host method declares its classification explicitly, and a test refuses one that declares none, so a new operation cannot inherit a default.
- **`images: [field…]`** names result fields that hold the path of a JPEG the owner wrote for display. It is set on `preview.read` and `preview.region`, and declared per job kind for those fields in a job's result.
- **Client identity and connected clients:**
  - `session.identify {label}` is session-scoped and names the calling client, 1–64 characters, for the desktop's list. `luxforge-ctl` identifies itself as "luxforge-ctl".
  - `session.clients` lists the owner's other registered clients: `{client_id, label?, authority, transport, connected_at, last_request_at?, requests}`. It reports metadata only, never the content of requests.
  - `session.disconnect {client_id}` closes that client's connection. Its session ends exactly as a disconnect ends it today: drafts end and `clients` jobs it alone wanted stop. Only permission authority may call it, which means the desktop. A connection or disconnection is recorded in the event log, so an open Settings tab follows it without polling.
  - The events are `session.connect`, `session.identify` and `session.disconnect`, each naming `client_id`; the [architecture's connected clients](architecture.md#agent-contract) give the transports, refusals and how a JSON-lines stream is closed.

## Settings › Agents

A tab in the Settings sheet, after Models and before Experiments, also opened from the palette (Settings · Agents). Its boards are drawn with the implementation, in this design's folder, at 1440 × 900 logical points like the other Settings boards.

- **Introduction.** One opening sentence: agents use the same operations you do, every edit is in history and undoable, and an agent can never grant consent or permissions.
- **Connect an agent.**
  - The tab shows the `luxforge-mcp` beside this build: `Contents/Helpers/luxforge-mcp` in the macOS bundle, or beside the editor elsewhere. A development build without one says it is missing and names the build command; nothing is drawn as if it worked.
  - A segmented choice of Claude Code, Claude Desktop, Codex, Cursor and Other (MCP JSON) shows that client's configuration with Copy, the file it goes in and one line saying what to do ([client configuration](#client-configuration)). For the bundled server:
    - Claude Code: `claude mcp add --scope user luxforge -- '/Applications/Luxforge.app/Contents/Helpers/luxforge-mcp'`, run in a terminal. The user scope makes Luxforge available in every project, not only the folder the command runs in.
    - Claude Desktop: the `mcpServers` block for `claude_desktop_config.json`, which Claude Desktop opens from Settings › Developer › Edit Config, then quit and reopen it.
    - Codex: the `[mcp_servers.luxforge]` table, with `command` and `args`, for `~/.codex/config.toml`.
    - Cursor: the `mcpServers` block, with `"type": "stdio"`, for `~/.cursor/mcp.json` or a project's `.cursor/mcp.json`.
    - Other: the same `mcpServers` block, with `command` and `args`, that most clients read.
  - Luxforge copies text to the system clipboard. It never edits another application's configuration.
  - The snippets come from one core function, also printed by `luxforge-mcp config CLIENT`, so the tab and the command line agree and the text is testable without the interface.
- **Connected.**
  - Every other live client is listed: its label (for example "Claude Code via MCP", "luxforge-ctl", or "Unnamed client"), how long it has been connected, its last activity and its request count.
  - Each row has **Disconnect** (`session.disconnect`).
  - With none connected, one line says so.
  - The list follows connections live while the tab is open.

There is no switch to turn live sessions off. That is [later work](#later).

## Statistics and privacy

The adapter adds no statistics. A call through a live session on an opted-in desktop is counted at the shared operation boundary, as a `luxforge-ctl` call is ([usage statistics](usage-statistics.md)), and usage events carry no client label. A headless owner never collects. Image attachment sends pixels only to the MCP client the person configured, never anywhere else.

## Performance and bounds

The adapter is a separate process that holds at most two live connections: one serving calls, and the event watch in a typed profile. The owner allows eight clients, and a ninth gets the owner's `resource-limit`.

| Bound | Value |
| --- | --- |
| Incoming line | 1 MiB |
| Queued requests | 32 |
| Calls in flight | 1 |
| Response | 64 MiB |
| Attached image | 8 MiB |
| Held wait | 30 s |

The adapter has no polling loop: waits are the owner's held `events.wait` and `job.wait`. The core additions are metadata on the static method table plus three small session methods; `session.clients` reads the owner's existing client table. Nothing renders, decodes or blocks on the catalog owner's thread.

## Implementation

What is built today: the live-session adapter with the compact profile, `config` with its client configuration, and packaging beside `luxforge-ctl`. The typed profiles, list-change notifications, headless mode and the `rmcp` interoperability test are not built yet.

### Command line

`luxforge-mcp [--catalog PATH] [--tools compact] [--no-images]`, with `--help` and `--version`, which answer without serving. `luxforge-mcp config CLIENT [--headless] [--catalog PATH]` prints one client's configuration for its own executable instead ([client configuration](#client-configuration)). `--tools` takes only `compact` today; any other value, and any other argument, is a usage error: `{"error": {code: "usage", message}}` on standard error and exit status 2. The server ends with exit status 0 when standard input ends: requests still queued are dropped, and the one being answered finishes first. Diagnostics (attaching, a closed or lost connection, an unreadable registry) go to standard error as plain lines, never the session token.

### Client configuration

`luxforge_core::agents` makes every client's configuration from the server's absolute path and its optional arguments; the desktop's Settings › Agents tab and `luxforge-mcp config` both call it, so they print the same text.

- **Input.** A `Launch`: the absolute path of `luxforge-mcp`, and optionally `--catalog PATH` (absolute), with `--headless` before it to start its own owner. A relative or non-Unicode path, and `--headless` without a catalog, are refused: a client starts the server from a folder of its own choosing, and no configuration format holds a non-Unicode path.
- **Output.** A `Snippet` per client: the client, the kind of text (a command, JSON or TOML), the text, the file it goes in on that platform and one instruction line. The server is always named `luxforge`.
- **Clients.**

  | Client (`config` id) | Text | Where it goes |
  | --- | --- | --- |
  | Claude Code (`claude-code`) | `claude mcp add --scope user luxforge -- PATH [ARGS]`: each path in single quotes, a quote inside escaped as POSIX shells read it on macOS and Linux, and doubled for PowerShell on Windows | A terminal |
  | Claude Desktop (`claude-desktop`) | `{"mcpServers": {"luxforge": {"command", "args"}}}` | `~/Library/Application Support/Claude/claude_desktop_config.json` on macOS, `%APPDATA%\Claude\claude_desktop_config.json` on Windows and `~/.config/Claude/claude_desktop_config.json` on Linux |
  | Codex (`codex`) | `[mcp_servers.luxforge]` with `command` and `args` as TOML basic strings | `~/.codex/config.toml` (`%USERPROFILE%\.codex\config.toml` on Windows) |
  | Cursor (`cursor`) | `{"mcpServers": {"luxforge": {"type": "stdio", "command", "args"}}}` | `~/.cursor/mcp.json` (`%USERPROFILE%\.cursor\mcp.json` on Windows), or a project's `.cursor/mcp.json` |
  | Other (`json`) | `{"mcpServers": {"luxforge": {"command", "args"}}}` | The client's own configuration |

  Anthropic documents Claude Desktop's configuration file for macOS and Windows; its Linux release is a beta, and the Linux path is the one its configuration folder takes there. The instruction names Settings › Developer › Edit Config, which opens the file on every platform.
- **Finding the server.** `agents::server_beside` is where it ships relative to the editor's own canonical executable: `Contents/Helpers/luxforge-mcp` in a macOS bundle, beside the editor otherwise (the Linux payload and a development build's target folder). Whether it exists is the caller's to check.
- **`config`.** `luxforge-mcp config CLIENT` passes its own canonical executable path (`current_exe`), and a `--catalog` made absolute from the current folder. It prints the snippet's text on standard output and where it goes and what to do on standard error, and writes no file. An unknown or missing client is a usage error listing the five ids, exit status 2.
- Nothing reads or writes another application's configuration.

### Packaging

`luxforge-mcp` is the fourth release executable (`EXECUTABLES` in `xtask/cli/src/release_candidate.rs`): `Contents/Helpers/luxforge-mcp` in the macOS bundle, signed with the other helpers before the app, and `Luxforge/luxforge-mcp` in the Linux payload, mode 0755 in both archives. The release build checks its `--version` against the tag, its hashes are bound in the build records before and after signing, the Linux record lists its ELF imports, and the Linux `INSTALL.txt` names it and its `config` command.

### Module layout

The library side is `luxforge_cli::mcp`; the binary is `src/luxforge_mcp.rs`.

- `protocol` reads one line as a JSON-RPC message and decides each request's era: modern when its `_meta` names `io.modelcontextprotocol/protocolVersion`, legacy after `initialize`. It builds results (a modern one gains `resultType: "complete"` and `_meta["io.modelcontextprotocol/serverInfo"]`), errors and notifications, the label and the actor.
- `serve` is the reader thread and the one worker over a bounded queue. The reader frames standard input, refuses an unreadable line, an over-long line or a full queue at once, and applies `notifications/cancelled` at once. The worker answers queued requests in arrival order through a `Handle`. Both write whole lines through one shared output.
- `adapter` is the `Handle`: discovery, `initialize`, `ping`, `subscriptions/listen`, `tools/list` and `tools/call`.
- `tools` holds the `Profile` (only `Compact` today) and the three compact tools. `luxforge_call`'s path (check, envelope, one send, job warning, pictures) is one function a typed profile reuses per method.
- `owner` is the seam to a catalog owner: a `Target` attaches (`LiveTarget` finds the live session and connects with `LiveClient::connect_as`) and a `Connection` carries calls (`LiveClient` implements it). `Owner` attaches lazily, ends a session whose connection failed, and warns `session-replaced` on the next attachment. A headless `luxforge-json` child is another `Target` and `Connection` pair, and a typed profile's event watch is a second `Target::attach`.
- `images` finds pictures through the `images` and `job_images` markers and reads them within the bound.
- `schema` generates the input schemas and the request check ([schema generation](#schema-generation)).

### Protocol details

- **Versions.** `server/discover` and a refusal list `["2026-07-28", "2025-11-25", "2025-06-18"]`. A request whose `_meta` names any other version, a legacy one included, is refused with `-32022` and `data: {supported, requested}`. A modern request missing `clientCapabilities`, and any request naming no version before `initialize`, is `-32602`. `initialize` answers the client's version when it is 2025-11-25 or 2025-06-18, and 2025-11-25 otherwise; a second `initialize` is `-32600`. A modern request is still served as one after `initialize`.
- **Methods.** Modern: `server/discover`, `subscriptions/listen`, `tools/list`, `tools/call`. Legacy: `initialize`, `ping`, `tools/list`, `tools/call`. Anything else is `-32601`. A `tools/list` cursor is `-32602`: the list has one page.
- **Caching.** `server/discover` and modern `tools/list` answer `ttlMs: 3600000` and `cacheScope: "private"`: the compact list is fixed for the life of the process.
- **Capabilities.** `{tools: {}}`, without `listChanged`, since the compact list never changes. `subscriptions/listen` is acknowledged with `notifications/subscriptions/acknowledged` honouring nothing (`notifications: {}`) and stays open, unanswered, until the client cancels it.
- **Instructions.** Discovery and `initialize` carry a short `instructions` text naming the three tools and saying that edits land in history under the agent's name.
- **Framing errors.** An unparsable line is `-32700` and a message that is not a JSON-RPC request object (a batch, a null or fractional `id`, non-object `params`) is `-32600`, each without an `id` unless one could be read. A line over 1 MiB is `-32600` with `data: {limit}`, unread. A request arriving while 32 wait is refused at once with the application code `-31000` and `data: {limit: 32}`, outside the JSON-RPC reserved range as the 2026-07-28 code policy asks.
- **Cancellation.** A cancelled queued request is removed and never answered. A cancelled running call stops at its next hold (after attaching, after the `asset.state` read, before the job read) and its answer is withheld; a call already sent to the owner, such as a held `job.wait`, finishes there and its answer is dropped.
- **Legacy results.** `structuredContent` is included only when the result is a JSON object, as 2025-11-25 requires; the text block always carries the JSON.

### The compact tools

- **`luxforge_methods`.** `{source, catalog | session, count, methods}`. `source` is `live` with the session's `catalog`, or `built-in` with `session`, the failure that left no session (`no-session`, or `usage` listing several). It tries to attach, so it reads the running editor's registry when there is one. Each entry is `{name, summary, mutates, session_scoped, read_only?, reversible?, network?, callable}`: the markers as the entry gives them, and `callable` false for `authority: "permissions"`. The summary is the notes up to the first `. ` or `; `, at most 240 characters. `namespace` keeps a name equal to it or starting with it and a dot, so `module.settings` works as well as `edit`. Names are listed sorted, as `schema.list` gives them.
- **`luxforge_describe`.** `{source, catalog | session, methods}`, each method once: `{name, notes, mutates, session_scoped, read_only?, reversible?, network?, callable, envelope, revision_of?, expected_revision?, session?, input_schema}`. `envelope` is `revision`, `request` or null; `expected_revision` is `optional` or `required` when the method has one. `input_schema` is the generated schema of `params`, without `expected_revision`, which `luxforge_call` takes beside `params`. `session` states a session-scoped method's lifetime. An unknown name, or more than 16, is a `validation` tool error.
- **`luxforge_call`.** `expected_revision` is given beside `params`, never inside them. A `validation` tool error names the argument in `data.parameter`: `method`, `params`, `expected_revision` or `params.<name>`. A method with `authority: "permissions"` is refused `forbidden` before anything is sent. The registry's own failures pass through unchanged.

### Warnings

A result's warnings are `ApiFailure`-shaped, `{code, message, job_id?, data?}`, in one trailing text block `{"warnings": [...]}` after the result and any pictures; `structuredContent` stays the method's result exactly. The codes are:

- `session-replaced`, `data: {catalog}`, on the first result after the adapter attached again.
- `job-released`, `data: {kind, status, ownership}`, when `job.read` says a job the call started belongs to its clients.
- `image-too-large`, `data: {field, path, bytes, limit}`, and `image-unreadable`, `data: {field, path}`, for a marked picture not attached.

### Identity

The label is the client's `clientInfo` title, else its name, with control characters removed and trimmed, shortened so that "*client* via MCP" is at most 64 characters. A client that gives neither is labelled "MCP client". The actor is `mcp:` and the name, cut at a character boundary to 128 bytes, or `mcp`. A modern request's own `clientInfo` names its actor; a legacy session's comes from `initialize`. The label is sent once, when the connection is made, from the request that attached.

### Connection loss

Before each call the adapter checks, without sending, whether the owner closed the idle connection. If it did, it attaches again before sending, so an edit after a desktop restart is applied normally with a `session-replaced` warning. A connection that fails while a call is on its way ends the session: a read reports `connection-lost`, and a mutation reports `outcome-unknown` with `data: {method, request_id}`. Neither is retried.

### Tests

Unit tests cover framing, era and version rules, the label and actor, the queue's bound and both cancellations over a scripted handler, picture markers and the file bound, and the compact tools over a scripted owner serving the built-in registry. The core's `agents` tests cover each client's text for the bundled path, each platform's file, headless and catalog arguments, and paths with spaces, quotes, backslashes and control characters read back by a JSON parser, a TOML parser and `/bin/sh`. The process tests (`tests/mcp/`) run the real binary against an in-process owner and `LocalServer`, with a proxy that cuts the connection after a forwarded edit. `config` prints exactly the core's text for its own path for every client. Every message the binary writes there is validated against `tests/mcp/spec/`'s vendored `schema.json` of its era, checked by the request it answers. The vendored files' source, commit, checksums and licence are in `THIRD_PARTY.md` beside them.

## Verification

- **Schema generation.**
  - Every method of the full registry, and of the developer registry, generates a schema that validates against the JSON Schema 2020-12 metaschema.
  - Generated schemas accept the registry's own example requests.
  - Each kind's mapping has its own test.
  - Validation uses the pinned `jsonschema` crate as a development dependency.
- **Wire conformance.**
  - Every message the adapter writes in its tests validates against the vendored `schema.json` of its era (2026-07-28 or 2025-11-25), kept beside the tests with their source and licence.
  - Process tests cover:
    - discovery and the legacy handshake;
    - version refusal;
    - both list shapes;
    - calls, conflicts, a held job wait, images, a draft across calls;
    - cancellation, queue overflow and a lost connection;
    - no session, several sessions and headless.
  - These tests run the real binary against an in-process owner and `LocalServer`, as the `luxforge-ctl` process tests do.
- **Independent client.** `rmcp`, pinned as a development dependency of one integration test, connects to the real binary in both eras: it lists tools, calls a read and an edit, and receives a list change.
- **Rendered journey.** A background `mcp` smoke scenario in the rendered tier:
  - It launches the hidden desktop and runs `luxforge-mcp` against its live session.
  - It makes an edit and checks the history entry's actor and the changed frame.
  - It fetches a preview image.
  - It captures Settings › Agents listing the client, then disconnects it.
- **Real clients.** Qualification with Claude Code on the owner's M4 against a packaged build, set up from the tab's snippet: edit a photo, look at it, undo it in the desktop. It also covers whichever of Claude Desktop and Codex are installed, plus native Linux. This is recorded with its scope.

## Acceptance

- An MCP client configured from the tab or the guide lists Luxforge's tools without Luxforge open, attaches when it opens, and can do the following through either profile, each result preserving the API's structure:
  - read the catalog and a photo's state;
  - make a revision-checked edit that appears in history under its name and undoes like any other;
  - follow a job to its end;
  - see the photograph.
- Modern and legacy clients both work. An unsupported version is refused as the specification requires.
- No tool can do what a live session cannot: no permission authority, no consent grants, no direct catalog writes, no retries of mutations.
- Settings › Agents shows working configuration for the bundled binary and the connected clients, and Disconnect ends a client's session.
- `luxforge-mcp` ships in the macOS bundle and the Linux tarball beside `luxforge-ctl`, signed with the other helpers.
- The user guide documents only what exists.

## Decisions

Owner decisions, taken while planning:

- **Tools.** Both shapes from one generator, with the compact profile as the default. `--tools all`, or a namespace list, offers one typed tool per method.
- **Pictures.** Preview results carry the picture inline by default, at most the 2048 px tier and never a full-resolution export. `--no-images` withholds them.
- **Target.** The running desktop's live session by default. `--headless --catalog PATH` starts the adapter's own `luxforge-json` child, and is refused when a desktop has that catalog open.
- **Desktop.** A Settings › Agents tab with the binary's path, per-client configuration to copy, and the connected live clients.

Recorded defaults the owner can change; each is delegated to the implementer within the stated contract:

- stdio only, with no HTTP listener;
- dual-era support of 2026-07-28, 2025-11-25 and 2025-06-18;
- a hand-written protocol layer, with `rmcp` used only as a test client;
- tool names with `.` mapped to `_`;
- `expected_revision` optional for asset-revision methods;
- the actor `mcp:<client>` and the label "*client* via MCP";
- no resources, prompts or tasks;
- the 32-request queue and 8 MiB image bound;
- **Disconnect** on each connected client, with `session.disconnect` limited to permission authority;
- the snippet set: Claude Code, Claude Desktop, Codex, Cursor and generic JSON.

The exact module layout, thread structure, internal error types, `ttlMs` value, warning shapes, the wording of tool descriptions beyond the registry's notes, and the tab's layout within the Settings sheet's conventions are the implementer's.

## Later

- Publishing machine-readable shapes for the `json` parameters, and result schemas (`outputSchema`).
- The MCP tasks extension for Luxforge jobs.
- Streamable HTTP for remote or sandboxed agents, with its own authentication.
- A switch to turn live sessions off, and per-agent restrictions such as read-only agents.
